//! ACP turn driver: supervise one delegated turn inside an external harness.
//!
//! The driver owns the inner loop of a delegated turn. It spawns the harness
//! process through `ProcessHost`, negotiates the ACP handshake, drives one
//! `session/prompt`, projects `session/update` evidence into the append-only
//! ledger, and settles the turn with the harness' final message.
//!
//! The harness owns model selection, its own tool definitions, and its own
//! context handling. Holon never dispatches harness tool calls locally and
//! never claims a confinement the harness does not provide.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, ensure, Context, Result};
use holon_acp::harness::{AcpHarnessSpec, LaunchRequest, SandboxMode};
use holon_acp::protocol::{
    ClientCapabilities, ContentBlock, InitializeParams, PromptParams, SessionLoadParams,
    SessionNewParams, SessionResumeParams, SessionUpdate, StopReason, ToolCall, ToolCallStatus,
    ToolCallUpdate,
};
use holon_acp::{
    AcpClient, AcpClientHandler, AcpError, CancelHandle, ElicitationRequest, PermissionOutcome,
    PermissionRequest,
};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::provider::{ModelBlock, ProviderBlockData};
use crate::system::process::{
    RegisteredProcessGroupProbe, RegisteredProcessGroupQuiescence, RunningProcess,
};
use crate::system::{
    CaptureSpec, EffectiveExecution, ProcessHost, ProcessPurpose, ProcessRequest,
    ProgramInvocation, StdioSpec, StopSignal,
};

/// Provider-data format marker for every ACP-projected block.
pub(crate) const ACP_PROVIDER_DATA_FORMAT: &str = "acp_v1";

/// How long a cancelled turn may take to settle before the process is stopped.
const CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a graceful stop may take before the process group is forced.
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the process group to report itself empty.
const PROCESS_GROUP_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);
/// Resident ACP sessions are retired after a bounded idle period.
const ACP_IDLE_RETIREMENT: Duration = Duration::from_secs(60);

/// Harness binding resolved for one ACP-driven agent.
#[derive(Debug, Clone)]
pub(crate) struct AcpDriverBinding {
    /// Harness profile name; resolves to a built-in or configured profile.
    pub profile: String,
    /// Role text delivered through the negotiated persona channel.
    pub role: Option<String>,
}

/// One decision taken by the static permission gate.
#[derive(Debug, Clone)]
pub(crate) struct AcpPermissionDecision {
    pub outcome: &'static str,
    pub reason: String,
    pub tool_call_id: Option<String>,
    pub kind: Option<String>,
    pub path: Option<String>,
}

impl AcpPermissionDecision {
    fn to_value(&self) -> Value {
        json!({
            "outcome": self.outcome,
            "reason": self.reason,
            "tool_call_id": self.tool_call_id,
            "kind": self.kind,
            "path": self.path,
        })
    }
}

/// Static gate for `session/request_permission`.
///
/// The gate never asks a human: an unattended turn must not hang on a prompt
/// the operator cannot see. Every decision is recorded with its reason.
struct AcpPermissionGate {
    roots: Vec<PathBuf>,
    writable: bool,
    process_execution_exposed: bool,
    decisions: Mutex<Vec<AcpPermissionDecision>>,
}

impl AcpPermissionGate {
    fn new(execution: &EffectiveExecution) -> Self {
        let mut roots = vec![execution.workspace.execution_root().to_path_buf()];
        roots.extend(
            execution
                .attached_workspaces
                .iter()
                .map(|(_, path)| path.clone()),
        );
        Self {
            roots,
            writable: matches!(
                execution.workspace.access_mode(),
                Some(crate::system::WorkspaceAccessMode::ExclusiveWrite)
            ),
            process_execution_exposed: execution.profile.process_execution_exposed,
            decisions: Mutex::new(Vec::new()),
        }
    }

    fn record(&self, decision: AcpPermissionDecision) {
        if let Ok(mut decisions) = self.decisions.lock() {
            decisions.push(decision);
        }
    }

    fn recorded(&self) -> Vec<Value> {
        self.decisions
            .lock()
            .map(|decisions| {
                decisions
                    .iter()
                    .map(AcpPermissionDecision::to_value)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Take the decisions recorded so far, leaving the gate empty.
    ///
    /// A resident gate outlives a single turn, so each turn must consume only
    /// its own decisions instead of re-reporting earlier ones.
    fn drain(&self) -> Vec<Value> {
        self.decisions
            .lock()
            .map(|mut decisions| {
                decisions
                    .drain(..)
                    .map(|decision| decision.to_value())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Decide one request against the bound execution scope.
    fn decide(&self, request: &PermissionRequest) -> PermissionOutcome {
        let tool_call_id = string_field(&request.tool_call, "toolCallId");
        let kind = string_field(&request.tool_call, "kind");
        let title = string_field(&request.tool_call, "title");
        let outside = candidate_paths(&request.tool_call)
            .into_iter()
            .find(|path| !self.path_is_bound(path));
        if let Some(path) = outside {
            return self.reject(
                request,
                AcpPermissionDecision {
                    outcome: "reject",
                    reason: "path_outside_bound_execution_root".to_string(),
                    tool_call_id,
                    kind,
                    path: Some(path),
                },
            );
        }

        if !self.writable && (is_write_kind(kind.as_deref()) || is_write_kind(title.as_deref())) {
            return self.reject(
                request,
                AcpPermissionDecision {
                    outcome: "reject",
                    reason: "bound_workspace_is_read_only".to_string(),
                    tool_call_id,
                    kind,
                    path: None,
                },
            );
        }

        if !self.process_execution_exposed
            && (is_process_kind(kind.as_deref()) || is_process_kind(title.as_deref()))
        {
            return self.reject(
                request,
                AcpPermissionDecision {
                    outcome: "reject",
                    reason: "process_execution_disabled_by_execution_profile".to_string(),
                    tool_call_id,
                    kind,
                    path: None,
                },
            );
        }

        match allow_once_option(request) {
            Some(option_id) => {
                self.record(AcpPermissionDecision {
                    outcome: "allow_once",
                    reason: "request_stays_inside_the_bound_execution_scope".to_string(),
                    tool_call_id,
                    kind,
                    path: None,
                });
                PermissionOutcome::Selected { option_id }
            }
            None => {
                self.record(AcpPermissionDecision {
                    outcome: "reject",
                    reason: "no_allow_option_in_request".to_string(),
                    tool_call_id,
                    kind,
                    path: None,
                });
                PermissionOutcome::Cancelled
            }
        }
    }

    fn reject(
        &self,
        request: &PermissionRequest,
        decision: AcpPermissionDecision,
    ) -> PermissionOutcome {
        self.record(decision);
        reject_option(request)
            .map(|option_id| PermissionOutcome::Selected { option_id })
            .unwrap_or(PermissionOutcome::Cancelled)
    }

    /// Whether a path stays inside one of the bound roots.
    fn path_is_bound(&self, path: &str) -> bool {
        let candidate = Path::new(path);
        if candidate
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            return false;
        }
        if candidate.is_absolute() {
            return self.roots.iter().any(|root| candidate.starts_with(root));
        }
        // The harness resolves relative paths against the bound cwd. Reject
        // traversal explicitly rather than assuming the peer normalizes it.
        true
    }
}

#[async_trait::async_trait]
impl AcpClientHandler for AcpPermissionGate {
    async fn request_permission(&self, request: PermissionRequest) -> PermissionOutcome {
        self.decide(&request)
    }

    async fn create_elicitation(
        &self,
        _request: ElicitationRequest,
    ) -> Result<Value, holon_acp::AcpError> {
        Err(holon_acp::AcpError::UnsupportedCapability {
            capability: "elicitation".to_string(),
        })
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

fn is_write_kind(kind: Option<&str>) -> bool {
    let Some(kind) = kind else { return false };
    let kind = kind.to_ascii_lowercase();
    [
        "edit",
        "write",
        "create",
        "delete",
        "move",
        "patch",
        "apply_patch",
        "notebook",
        "rename",
    ]
    .iter()
    .any(|candidate| kind.contains(candidate))
}

fn is_process_kind(kind: Option<&str>) -> bool {
    let Some(kind) = kind else { return false };
    let kind = kind.to_ascii_lowercase();
    ["execute", "terminal", "command", "shell", "run"]
        .iter()
        .any(|candidate| kind.contains(candidate))
}

/// Candidate file paths a harness request refers to.
fn candidate_paths(tool_call: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let mut push_value = |value: &Value| {
        if let Some(path) = value.as_str() {
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    };
    if let Some(raw_input) = tool_call
        .get("rawInput")
        .or_else(|| tool_call.get("raw_input"))
    {
        for key in ["path", "file_path", "filePath", "paths", "files"] {
            match raw_input.get(key) {
                Some(Value::Array(items)) => items.iter().for_each(|item| push_value(item)),
                Some(value) => push_value(value),
                None => {}
            }
        }
    }
    if let Some(Value::Array(locations)) = tool_call.get("locations") {
        for location in locations {
            push_value(&location.get("path").cloned().unwrap_or(Value::Null));
        }
    }
    paths
}

/// The request option that grants a single execution.
fn allow_once_option(request: &PermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.option_id.eq_ignore_ascii_case("allow_once"))
        .or_else(|| {
            request.options.iter().find(|option| {
                let id = option.option_id.to_ascii_lowercase();
                let name = option.name.to_ascii_lowercase();
                (id.contains("allow")
                    || id.contains("approve")
                    || name.contains("allow")
                    || name.contains("approve"))
                    && (id.contains("once") || name.contains("once"))
            })
        })
        .map(|option| option.option_id.clone())
}

/// The request option that denies a single execution.
fn reject_option(request: &PermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| {
            let id = option.option_id.to_ascii_lowercase();
            let name = option.name.to_ascii_lowercase();
            id.contains("reject") || id.contains("deny") || name.contains("reject")
        })
        .map(|option| option.option_id.clone())
}

/// Ledger projection of one ACP turn.
pub(crate) struct AcpTurnProjection {
    /// Canonical model blocks for the settled turn.
    pub blocks: Vec<ModelBlock>,
    /// Final assistant text produced by the harness.
    pub text: String,
    /// Session-level context and cost evidence.
    pub usage: Option<Value>,
    /// Non-model evidence (plans, capabilities, unknown variants).
    pub evidence: Vec<Value>,
}

impl AcpTurnProjection {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            text: String::new(),
            usage: None,
            evidence: Vec::new(),
        }
    }
}

/// Project one `session/update` stream into ledger blocks and evidence.
fn project_updates(updates: &[SessionUpdate]) -> AcpTurnProjection {
    let mut projection = AcpTurnProjection::new();
    let mut message = String::new();
    for update in updates {
        match update {
            SessionUpdate::AgentMessageChunk { content } => {
                if let Some(text) = content.as_text() {
                    message.push_str(text);
                    if let Some(ModelBlock::Text { text: previous }) = projection.blocks.last_mut()
                    {
                        previous.push_str(text);
                    } else {
                        projection.blocks.push(ModelBlock::Text {
                            text: text.to_string(),
                        });
                    }
                }
            }
            SessionUpdate::AgentThoughtChunk { content } => {
                if let Some(text) = content.as_text() {
                    projection.blocks.push(ModelBlock::ReasoningText {
                        text: text.to_string(),
                    });
                }
            }
            SessionUpdate::ToolCall { tool_call } => {
                projection.blocks.push(tool_use_block(tool_call));
            }
            SessionUpdate::ToolCallUpdate { tool_call_update } => {
                projection.blocks.push(tool_result_block(tool_call_update));
            }
            SessionUpdate::UsageUpdate { used, size, cost } => {
                projection.usage = Some(json!({
                    "used": used,
                    "size": size,
                    "cost": cost,
                    "scope": "acp_session_context_window",
                }));
            }
            SessionUpdate::Plan { entries } => {
                projection.evidence.push(json!({
                    "format": ACP_PROVIDER_DATA_FORMAT,
                    "kind": "plan",
                    "entries": entries,
                }));
            }
            SessionUpdate::CurrentModeUpdate { current_mode_id } => {
                projection.evidence.push(json!({
                    "format": ACP_PROVIDER_DATA_FORMAT,
                    "kind": "current_mode",
                    "mode_id": current_mode_id,
                }));
            }
            SessionUpdate::AvailableCommandsUpdate { available_commands } => {
                projection.evidence.push(json!({
                    "format": ACP_PROVIDER_DATA_FORMAT,
                    "kind": "available_commands",
                    "commands": available_commands,
                }));
            }
            SessionUpdate::Unknown => {
                projection.evidence.push(json!({
                    "format": ACP_PROVIDER_DATA_FORMAT,
                    "kind": "unknown_session_update",
                }));
            }
        }
    }
    projection.text = message.trim().to_string();
    projection
}

fn tool_use_block(tool_call: &ToolCall) -> ModelBlock {
    let name = tool_call
        .kind
        .clone()
        .or_else(|| tool_call.title.clone())
        .unwrap_or_else(|| "acp_tool".to_string());
    ModelBlock::ProviderToolUse {
        id: tool_call.tool_call_id.clone(),
        name,
        input: tool_call.raw_input.clone().unwrap_or(Value::Null),
        status: tool_call
            .status
            .as_ref()
            .map(status_label)
            .map(|label| label.to_string()),
        provider_data: Some(ProviderBlockData {
            format: ACP_PROVIDER_DATA_FORMAT.to_string(),
            payload: serde_json::to_value(tool_call).unwrap_or(Value::Null),
        }),
    }
}

fn tool_result_block(tool_call_update: &ToolCallUpdate) -> ModelBlock {
    let content = tool_call_update
        .raw_output
        .clone()
        .or_else(|| tool_call_update.content.clone())
        .unwrap_or(Value::Null);
    ModelBlock::ProviderToolResult {
        tool_use_id: Some(tool_call_update.tool_call_id.clone()),
        content,
        is_error: matches!(tool_call_update.status, Some(ToolCallStatus::Failed)),
        status: tool_call_update
            .status
            .as_ref()
            .map(status_label)
            .map(|label| label.to_string()),
        provider_data: Some(ProviderBlockData {
            format: ACP_PROVIDER_DATA_FORMAT.to_string(),
            payload: serde_json::to_value(tool_call_update).unwrap_or(Value::Null),
        }),
    }
}

fn status_label(status: &ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "in_progress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        ToolCallStatus::Unknown => "unknown",
    }
}

/// Stop reason of one ACP turn as recorded in the ledger.
fn stop_reason_label(stop_reason: &StopReason) -> &'static str {
    match stop_reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        StopReason::Cancelled => "cancelled",
        StopReason::Unknown => "unknown",
    }
}

/// The resolved harness profile for one ACP turn.
struct ResolvedProfile {
    spec: AcpHarnessSpec,
    role: Option<String>,
    env: BTreeMap<String, String>,
    sandbox: SandboxMode,
}

/// Resolve the harness profile, its overrides, and the injected environment.
fn resolve_profile(
    config: &crate::config::AcpConfigFile,
    binding: &AcpDriverBinding,
    execution: &EffectiveExecution,
) -> Result<ResolvedProfile> {
    let registry = holon_acp::AcpHarnessRegistry::with_builtin_profiles();
    let mut spec = registry
        .get(&binding.profile)
        .cloned()
        .ok_or_else(|| anyhow!("unknown ACP harness profile `{}`", binding.profile))?;

    let overrides = config.profile(&binding.profile);
    if let Some(overrides) = overrides {
        if let Some(program) = &overrides.program {
            spec.program = program.clone();
        }
        if let Some(args) = &overrides.args {
            let mut merged = args.clone();
            merged.extend(spec.default_args.iter().cloned());
            spec.default_args = merged;
        }
    }

    let role = binding
        .role
        .clone()
        .or_else(|| overrides.and_then(|profile| profile.role.clone()));

    let sandbox = match overrides.and_then(|profile| profile.sandbox.clone()) {
        Some(value) => parse_sandbox_mode(&value)?,
        None => default_sandbox_mode(execution),
    };

    let mut env = spec.filter_env(&current_process_env());
    if let Some(overrides) = overrides {
        for (name, value) in &overrides.env {
            env.insert(name.clone(), value.clone());
        }
    }

    Ok(ResolvedProfile {
        spec,
        role,
        env,
        sandbox,
    })
}

/// The sandbox mode implied by the bound execution scope.
fn default_sandbox_mode(execution: &EffectiveExecution) -> SandboxMode {
    match execution.workspace.access_mode() {
        Some(crate::system::WorkspaceAccessMode::SharedRead) => SandboxMode::ReadOnly,
        Some(crate::system::WorkspaceAccessMode::ExclusiveWrite) => SandboxMode::WorkspaceWrite,
        None => SandboxMode::FullAccess,
    }
}

fn parse_sandbox_mode(value: &str) -> Result<SandboxMode> {
    match value {
        "read_only" => Ok(SandboxMode::ReadOnly),
        "workspace_write" => Ok(SandboxMode::WorkspaceWrite),
        "full_access" => Ok(SandboxMode::FullAccess),
        other => Err(anyhow!("unknown ACP sandbox mode `{other}`")),
    }
}

/// The host environment the harness allowlist is applied to.
fn current_process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

/// The host environment the harness process receives.
fn harness_env(env: &BTreeMap<String, String>) -> Vec<(String, String)> {
    env.iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Prompt text handed to the harness for one delegated turn.
///
/// The handoff carries task-level content only. Holon framing, tool
/// descriptions, and workspace `AGENTS.md` never travel to the harness.
fn handoff_prompt(prompt: &str, role: Option<&str>) -> String {
    match role {
        Some(role) if !role.trim().is_empty() => {
            format!("{}\n\nRole expectations:\n{}", prompt.trim(), role.trim())
        }
        _ => prompt.trim().to_string(),
    }
}

/// Runtime handle for a live ACP session that can still be cancelled.
struct AcpSessionControl {
    cancel: CancelHandle,
    process: Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>>,
    group: Option<Arc<dyn RegisteredProcessGroupProbe>>,
}

impl AcpSessionControl {
    /// Cancel the session, then escalate to a graceful and forced stop.
    ///
    /// The escalation never claims the group is empty: the proof comes from
    /// the registered process-group probe.
    async fn cancel_and_reap(&self) {
        let _ = self.cancel.cancel().await;
        reap_process(&self.process, self.group.as_ref()).await;
    }
}

fn process_group_is_empty(group: Option<&Arc<dyn RegisteredProcessGroupProbe>>) -> bool {
    group.is_some_and(|group| group.observe() == RegisteredProcessGroupQuiescence::Empty)
}

async fn reap_process(
    process: &Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>>,
    group: Option<&Arc<dyn RegisteredProcessGroupProbe>>,
) {
    if process_group_is_empty(group) {
        return;
    }
    {
        let mut process = process.lock().await;
        let _ = process.stop(StopSignal::Graceful).await;
    }
    let graceful_deadline = tokio::time::Instant::now() + GRACEFUL_STOP_TIMEOUT;
    while !process_group_is_empty(group) && tokio::time::Instant::now() < graceful_deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if process_group_is_empty(group) {
        return;
    }
    {
        let mut process = process.lock().await;
        let _ = process.stop(StopSignal::Force).await;
    }
    let deadline = tokio::time::Instant::now() + PROCESS_GROUP_QUIESCENCE_TIMEOUT;
    while !process_group_is_empty(group) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Build the launch command and environment for one harness process.
fn launch_request(
    profile: &ResolvedProfile,
    execution: &EffectiveExecution,
) -> Result<(LaunchRequest, Vec<(String, String)>)> {
    let root = execution.workspace.execution_root().to_path_buf();
    let additional_directories = execution
        .attached_workspaces
        .iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let request = LaunchRequest {
        workspace_dir: root.to_string_lossy().into_owned(),
        additional_directories,
        sandbox: profile.sandbox,
        // `--agent` may only reference a pre-existing named agent. The runtime
        // never materializes a temporary `agent.md` inside a managed worktree.
        agent: None,
        log_file: None,
        disable_telemetry: false,
    };
    Ok((request, harness_env(&profile.env)))
}

/// Session metadata that never enters the model prompt.
fn session_meta(profile: &ResolvedProfile) -> Map<String, Value> {
    profile
        .spec
        .session_meta(&holon_acp::harness::SessionOptions::default())
}

/// Whether the peer declared the negotiated system-prompt channel.
fn supports_system_prompt(handshake: &holon_acp::Handshake) -> bool {
    handshake
        .agent_capabilities
        .session_capabilities
        .system_prompt
        .is_some()
}

/// The role text delivered through the negotiated persona channel.
fn negotiated_role(profile: &ResolvedProfile, handshake: &holon_acp::Handshake) -> Option<String> {
    if !supports_system_prompt(handshake) {
        return None;
    }
    profile.role.clone()
}

/// Spawn the harness process and bridge its stdio to the ACP client.
async fn spawn_harness(
    runtime: &super::RuntimeHandle,
    profile: &ResolvedProfile,
    execution: &EffectiveExecution,
    gate: Arc<AcpPermissionGate>,
) -> Result<SpawnedHarness> {
    #[cfg(test)]
    if let Some(harness) = runtime.test_acp_harness(gate.clone()).await {
        return Ok(harness);
    }
    let (request, env) = launch_request(profile, execution)?;
    let launch = profile.spec.build_launch(&request);
    ensure!(
        !launch
            .args
            .iter()
            .any(|arg| { arg.contains("dangerously-skip-permissions") || arg.contains("yolo") }),
        "ACP harness launch must not disable the harness permission gate"
    );

    let system = runtime.system();
    let scope = super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.inner.mutation_owner.agent_id().to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let responsibility = super::command_task::OriginalManagedProcess::before_spawn(
        &scope,
        &runtime.inner.input_evidence_budget,
    )?;
    runtime
        .inner
        .control_cancellations
        .retain_managed_process(responsibility.clone(), &runtime.inner.input_evidence_budget)?;

    let mut process = system
        .spawn(
            execution,
            ProcessRequest {
                program: ProgramInvocation::Argv {
                    program: launch.program.clone(),
                    args: launch.args.iter().map(OsString::from).collect(),
                },
                cwd: Some(execution.workspace.execution_root().to_path_buf()),
                env,
                stdin: StdioSpec::Piped,
                tty: false,
                capture: CaptureSpec::BOTH,
                timeout: None,
                purpose: ProcessPurpose::CommandTask,
            },
        )
        .await
        .map_err(|error| {
            responsibility.record_not_started();
            error
        })?;
    responsibility.attach_original_handle(process.as_ref())?;
    let group = process.registered_group_probe();
    let stdout = process
        .take_stdout()
        .ok_or_else(|| anyhow!("ACP harness stdout is not available"))?;
    let stdin = process
        .take_stdin()
        .ok_or_else(|| anyhow!("ACP harness stdin is not available"))?;
    if let Some(stderr) = process.take_stderr() {
        tokio::spawn(async move {
            let mut stderr = stderr;
            let mut buffer = [0u8; 1024];
            loop {
                match tokio::io::AsyncReadExt::read(&mut stderr, &mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
    }
    let process = Arc::new(tokio::sync::Mutex::new(process));
    let client = Arc::new(AcpClient::from_split(
        stdout,
        stdin,
        gate as Arc<dyn AcpClientHandler>,
    ));
    Ok(SpawnedHarness {
        client,
        process,
        group,
    })
}

/// A harness substituted by a test, standing in for a spawned process.
///
/// The seam keeps the continuation protocol under test without needing a real
/// `agy acp` peer; production builds never compile it.
#[cfg(test)]
pub(crate) struct AcpTestHarness {
    pub(crate) client: Arc<AcpClient>,
    pub(crate) process: Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>>,
    pub(crate) group: Option<Arc<dyn RegisteredProcessGroupProbe>>,
}

#[cfg(test)]
impl From<AcpTestHarness> for SpawnedHarness {
    fn from(harness: AcpTestHarness) -> Self {
        Self {
            client: harness.client,
            process: harness.process,
            group: harness.group,
        }
    }
}

#[cfg(test)]
pub(crate) type AcpTestHarnessFactory =
    Arc<dyn Fn(Arc<dyn AcpClientHandler>) -> AcpTestHarness + Send + Sync>;

/// A resident harness process and its bridged ACP client.
pub(crate) struct AcpResidentSession {
    pub(crate) client: Arc<AcpClient>,
    gate: Arc<AcpPermissionGate>,
    pub(crate) process: Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>>,
    pub(crate) group: Option<Arc<dyn RegisteredProcessGroupProbe>>,
    pub(crate) session_id: String,
    pub(crate) profile: String,
    pub(crate) bound_execution_root: String,
    pub(crate) bound_additional_dirs: Vec<String>,
    /// Permission posture the resident connection was opened with. The ACP
    /// handler is bound at connect time, so a reuse must re-check it instead of
    /// inheriting a gate from an earlier, more permissive execution profile.
    bound_writable: bool,
    bound_process_execution_exposed: bool,
    pub(crate) last_used: Instant,
}

/// Whether a resident harness process is still running.
async fn acp_resident_is_alive(session: &AcpResidentSession) -> bool {
    let mut process = session.process.lock().await;
    matches!(process.try_status().await, Ok(None))
}

/// A spawned harness process and its bridged ACP client.
struct SpawnedHarness {
    client: Arc<AcpClient>,
    process: Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>>,
    group: Option<Arc<dyn RegisteredProcessGroupProbe>>,
}

struct AcpSessionAcquire {
    session: AcpResidentSession,
    new_session: bool,
    rebuilt_session: bool,
    prompt_role: Option<String>,
}

fn execution_binding(execution: &EffectiveExecution) -> (String, Vec<String>) {
    let root = execution
        .workspace
        .execution_root_id()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            execution
                .workspace
                .execution_root()
                .to_string_lossy()
                .into_owned()
        });
    let additional_dirs = execution
        .attached_workspaces
        .iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect();
    (root, additional_dirs)
}

fn binding_matches(
    profile: &str,
    bound_execution_root: &str,
    bound_additional_dirs: &[String],
    expected_profile: &str,
    expected_execution_root: &str,
    expected_additional_dirs: &[String],
) -> bool {
    profile == expected_profile
        && bound_execution_root == expected_execution_root
        && bound_additional_dirs == expected_additional_dirs
}

fn session_resume_params(
    session_id: &str,
    execution: &EffectiveExecution,
    profile: &ResolvedProfile,
) -> SessionResumeParams {
    let mut params = SessionResumeParams::new(session_id);
    params.cwd = Some(
        execution
            .workspace
            .execution_root()
            .to_string_lossy()
            .into_owned(),
    );
    params.additional_directories = execution_binding(execution).1;
    params.meta = Some(session_meta(profile));
    params
}

fn session_load_params(
    session_id: &str,
    execution: &EffectiveExecution,
    profile: &ResolvedProfile,
) -> SessionLoadParams {
    let mut params = SessionLoadParams::new(session_id);
    params.cwd = Some(
        execution
            .workspace
            .execution_root()
            .to_string_lossy()
            .into_owned(),
    );
    params.additional_directories = execution_binding(execution).1;
    params.meta = Some(session_meta(profile));
    params
}

fn session_new_params(
    execution: &EffectiveExecution,
    profile: &ResolvedProfile,
    handshake: &holon_acp::Handshake,
) -> SessionNewParams {
    let mut params = SessionNewParams::new(
        execution
            .workspace
            .execution_root()
            .to_string_lossy()
            .into_owned(),
    );
    params.additional_directories = execution_binding(execution).1;
    params.meta = Some(session_meta(profile));
    params.system_prompt = negotiated_role(profile, handshake);
    params
}

fn bounded_recovery_summary(state: &crate::types::AgentState) -> Option<String> {
    state
        .last_turn_terminal
        .as_ref()
        .and_then(|terminal| terminal.last_assistant_message.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.chars().take(2048).collect())
}

fn recovery_handoff_prompt(prompt: &str, role: Option<&str>, summary: Option<&str>) -> String {
    let mut handoff = String::new();
    if let Some(summary) = summary.filter(|summary| !summary.is_empty()) {
        handoff.push_str("Bounded handoff from the previous ACP session:\n");
        handoff.push_str(summary);
        handoff.push_str("\n\n");
    }
    handoff.push_str(&handoff_prompt(prompt, role));
    handoff
}

#[cfg(test)]
impl super::RuntimeHandle {
    /// Install the harness a test drives instead of spawning a real peer.
    pub(crate) async fn install_acp_test_harness(&self, factory: AcpTestHarnessFactory) {
        *self.inner.acp_test_harness.lock().await = Some(factory);
    }

    async fn test_acp_harness(&self, gate: Arc<AcpPermissionGate>) -> Option<SpawnedHarness> {
        let factory = self.inner.acp_test_harness.lock().await.clone()?;
        Some(factory(gate as Arc<dyn AcpClientHandler>).into())
    }
}

impl super::RuntimeHandle {
    /// Retire a persisted upstream session that can no longer be continued.
    ///
    /// The rotation is recorded before the credential is dropped so the
    /// degraded `session/new` path stays auditable.
    async fn rotate_acp_session_state(
        &self,
        binding: &AcpDriverBinding,
        previous_session_id: &str,
        reason: &str,
    ) -> Result<()> {
        self.append_audit_event(
            "acp_session_rotated",
            json!({
                "driver": "acp",
                "acp_profile": binding.profile,
                "previous_session_id": previous_session_id,
                "reason": reason,
            }),
        )
        .await
        .context("record ACP session rotation")?;
        self.clear_acp_session_state().await
    }

    async fn persist_acp_session_state(
        &self,
        profile: &str,
        session_id: &str,
        bound_execution_root: &str,
        bound_additional_dirs: &[String],
    ) -> Result<()> {
        let profile = profile.to_string();
        let session_id = session_id.to_string();
        let bound_execution_root = bound_execution_root.to_string();
        let bound_additional_dirs = bound_additional_dirs.to_vec();
        self.update_agent_state(|state| {
            state.acp_session_state = Some(Box::new(crate::types::AcpSessionState {
                acp_profile: profile,
                resume_session_id: session_id,
                bound_execution_root,
                bound_additional_dirs,
            }));
            Ok(())
        })
        .await?;
        Ok(())
    }

    async fn clear_acp_session_state(&self) -> Result<()> {
        self.update_agent_state(|state| {
            state.acp_session_state = None;
            Ok(())
        })
        .await?;
        Ok(())
    }

    async fn acquire_acp_session(
        &self,
        profile: &ResolvedProfile,
        binding: &AcpDriverBinding,
        execution: &EffectiveExecution,
        gate: Arc<AcpPermissionGate>,
    ) -> Result<AcpSessionAcquire> {
        let (bound_execution_root, bound_additional_dirs) = execution_binding(execution);
        if let Some(resident) = self.inner.acp_session.lock().await.take() {
            if binding_matches(
                &resident.profile,
                &resident.bound_execution_root,
                &resident.bound_additional_dirs,
                &binding.profile,
                &bound_execution_root,
                &bound_additional_dirs,
            ) && resident.bound_writable == gate.writable
                && resident.bound_process_execution_exposed == gate.process_execution_exposed
                && acp_resident_is_alive(&resident).await
            {
                return Ok(AcpSessionAcquire {
                    session: *resident,
                    new_session: false,
                    rebuilt_session: false,
                    prompt_role: None,
                });
            }
            // The connection is dead, or the execution profile no longer grants
            // the permission posture it was opened with. Either way the handler
            // cannot be replaced, so drop the resident and rebuild it; the
            // persisted session id still resumes below.
            reap_process(&resident.process, resident.group.as_ref()).await;
        }

        let state = self.agent_state().await?;
        let persisted = state.acp_session_state.clone();
        let persisted_matches = persisted.as_ref().is_some_and(|session| {
            binding_matches(
                &session.acp_profile,
                &session.bound_execution_root,
                &session.bound_additional_dirs,
                &binding.profile,
                &bound_execution_root,
                &bound_additional_dirs,
            )
        });
        let had_persisted_session = persisted.is_some();
        if had_persisted_session && !persisted_matches {
            self.clear_acp_session_state().await?;
        }

        let spawned = spawn_harness(self, profile, execution, gate.clone()).await?;
        let SpawnedHarness {
            client,
            process,
            group,
        } = spawned;
        let handshake = match client
            .initialize(InitializeParams::new(ClientCapabilities::trimmed()))
            .await
        {
            Ok(handshake) => handshake,
            Err(error) => {
                reap_process(&process, group.as_ref()).await;
                return Err(anyhow!(error).context("ACP initialize failed"));
            }
        };

        let mut new_session = !persisted_matches;
        let mut rebuilt_session = had_persisted_session && !persisted_matches;
        let session = if let Some(persisted) = persisted.filter(|_| persisted_matches) {
            if handshake.supports_resume() {
                match client
                    .resume_session(session_resume_params(
                        &persisted.resume_session_id,
                        execution,
                        profile,
                    ))
                    .await
                {
                    Ok(session) => session,
                    Err(AcpError::SessionNotFound { .. }) => {
                        rebuilt_session = true;
                        new_session = true;
                        self.rotate_acp_session_state(
                            binding,
                            &persisted.resume_session_id,
                            "resume_session_not_found",
                        )
                        .await?;
                        client
                            .new_session(session_new_params(execution, profile, &handshake))
                            .await
                            .map_err(|error| anyhow!(error).context("ACP session/new failed"))?
                    }
                    Err(AcpError::UnsupportedByPeer { .. }) if handshake.supports_load() => {
                        new_session = false;
                        match client
                            .load_session(session_load_params(
                                &persisted.resume_session_id,
                                execution,
                                profile,
                            ))
                            .await
                        {
                            Ok(session) => {
                                client
                                    .drain_session_updates(&persisted.resume_session_id)
                                    .await;
                                session
                            }
                            Err(AcpError::SessionNotFound { .. }) => {
                                rebuilt_session = true;
                                new_session = true;
                                self.rotate_acp_session_state(
                                    binding,
                                    &persisted.resume_session_id,
                                    "load_session_not_found",
                                )
                                .await?;
                                client
                                    .new_session(session_new_params(execution, profile, &handshake))
                                    .await
                                    .map_err(|error| {
                                        anyhow!(error).context("ACP session/new failed")
                                    })?
                            }
                            Err(error) => {
                                reap_process(&process, group.as_ref()).await;
                                return Err(anyhow!(error).context("ACP session/load failed"));
                            }
                        }
                    }
                    Err(error) => {
                        reap_process(&process, group.as_ref()).await;
                        return Err(anyhow!(error).context("ACP session/resume failed"));
                    }
                }
            } else if handshake.supports_load() {
                new_session = false;
                match client
                    .load_session(session_load_params(
                        &persisted.resume_session_id,
                        execution,
                        profile,
                    ))
                    .await
                {
                    Ok(session) => {
                        client
                            .drain_session_updates(&persisted.resume_session_id)
                            .await;
                        session
                    }
                    Err(AcpError::SessionNotFound { .. }) => {
                        rebuilt_session = true;
                        new_session = true;
                        self.rotate_acp_session_state(
                            binding,
                            &persisted.resume_session_id,
                            "load_session_not_found",
                        )
                        .await?;
                        client
                            .new_session(session_new_params(execution, profile, &handshake))
                            .await
                            .map_err(|error| anyhow!(error).context("ACP session/new failed"))?
                    }
                    Err(error) => {
                        reap_process(&process, group.as_ref()).await;
                        return Err(anyhow!(error).context("ACP session/load failed"));
                    }
                }
            } else {
                rebuilt_session = true;
                new_session = true;
                self.rotate_acp_session_state(
                    binding,
                    &persisted.resume_session_id,
                    "peer_session_unsupported",
                )
                .await?;
                client
                    .new_session(session_new_params(execution, profile, &handshake))
                    .await
                    .map_err(|error| anyhow!(error).context("ACP session/new failed"))?
            }
        } else {
            client
                .new_session(session_new_params(execution, profile, &handshake))
                .await
                .map_err(|error| anyhow!(error).context("ACP session/new failed"))?
        };

        if let Err(error) = self
            .persist_acp_session_state(
                &binding.profile,
                &session.session_id,
                &bound_execution_root,
                &bound_additional_dirs,
            )
            .await
        {
            reap_process(&process, group.as_ref()).await;
            return Err(error.context("persist ACP session state"));
        }

        let bound_writable = gate.writable;
        let bound_process_execution_exposed = gate.process_execution_exposed;
        Ok(AcpSessionAcquire {
            session: AcpResidentSession {
                client,
                gate,
                process,
                group,
                session_id: session.session_id,
                profile: binding.profile.clone(),
                bound_execution_root,
                bound_additional_dirs,
                bound_writable,
                bound_process_execution_exposed,
                last_used: Instant::now(),
            },
            new_session,
            rebuilt_session,
            prompt_role: if new_session && !supports_system_prompt(&handshake) {
                profile.role.clone()
            } else {
                None
            },
        })
    }

    async fn store_acp_session(&self, mut session: AcpResidentSession) {
        let session_id = session.session_id.clone();
        session.last_used = Instant::now();
        *self.inner.acp_session.lock().await = Some(Box::new(session));
        self.schedule_acp_idle_retirement(session_id);
    }

    fn schedule_acp_idle_retirement(&self, session_id: String) {
        let runtime = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(ACP_IDLE_RETIREMENT).await;
            let Some(inner) = runtime.upgrade() else {
                return;
            };
            let runtime = super::RuntimeHandle { inner };
            runtime.retire_acp_session_if_idle(&session_id).await;
        });
    }

    async fn retire_acp_session_if_idle(&self, expected_session_id: &str) {
        let resident = {
            let mut slot = self.inner.acp_session.lock().await;
            let retire = slot.as_ref().is_some_and(|session| {
                session.session_id == expected_session_id
                    && session.last_used.elapsed() >= ACP_IDLE_RETIREMENT
            });
            if retire {
                slot.take()
            } else {
                None
            }
        };
        if let Some(resident) = resident {
            reap_process(&resident.process, resident.group.as_ref()).await;
        }
    }

    pub(crate) async fn retire_acp_session(&self, _reason: &str) {
        let resident = self.inner.acp_session.lock().await.take();
        if let Some(resident) = resident {
            reap_process(&resident.process, resident.group.as_ref()).await;
        }
    }

    /// Run one delegated turn inside an external ACP harness.
    ///
    /// The turn is bounded: the harness answers with a single terminal message
    /// per `session/prompt`, and the driver settles the turn with that text.
    pub(crate) async fn run_acp_turn(
        &self,
        agent_id: &str,
        prompt: &str,
        execution: &EffectiveExecution,
        binding: &AcpDriverBinding,
        cancel: CancellationToken,
    ) -> Result<AcpTurnResult> {
        let config = self.inner.config_snapshot.load().acp.clone();
        let acp_config = &config;
        let profile = resolve_profile(acp_config, binding, execution)?;
        let gate = Arc::new(AcpPermissionGate::new(execution));
        let previous_state = self.agent_state().await?;
        let acquired = self
            .acquire_acp_session(&profile, binding, execution, gate)
            .await?;
        let AcpSessionAcquire {
            mut session,
            rebuilt_session,
            prompt_role,
            ..
        } = acquired;
        let session_id = session.session_id.clone();
        let control = AcpSessionControl {
            cancel: session.client.cancel_handle(session_id.clone()),
            process: Arc::clone(&session.process),
            group: session.group.clone(),
        };

        let handoff = recovery_handoff_prompt(
            prompt,
            prompt_role.as_deref(),
            rebuilt_session
                .then(|| bounded_recovery_summary(&previous_state))
                .flatten()
                .as_deref(),
        );
        let mut updates: Vec<SessionUpdate> = Vec::new();
        let outcome = {
            let prompt_future = session
                .client
                .prompt(PromptParams::text(session_id.clone(), handoff), |update| {
                    updates.push(update.clone())
                });
            tokio::pin!(prompt_future);
            tokio::select! {
                result = &mut prompt_future => Outcome::Settled(result),
                _ = cancel.cancelled() => Outcome::Cancelled,
            }
        };

        let projection = project_updates(&updates);
        let stop_reason = match &outcome {
            Outcome::Settled(Ok(result)) => stop_reason_label(&result.stop_reason).to_string(),
            Outcome::Settled(Err(_)) => "acp_error".to_string(),
            Outcome::Cancelled => "cancelled".to_string(),
        };
        if !matches!(&outcome, Outcome::Settled(Ok(_))) {
            control.cancel_and_reap().await;
        }
        let transcript_entry_id = match self
            .persist_acp_evidence(
                agent_id,
                &projection,
                &stop_reason,
                session.gate.drain(),
                session_id.as_str(),
            )
            .await
        {
            Ok(entry_id) => entry_id,
            Err(error) => {
                if matches!(&outcome, Outcome::Settled(Ok(_))) {
                    control.cancel_and_reap().await;
                }
                return Err(error);
            }
        };

        match outcome {
            Outcome::Settled(Ok(_)) => {
                session.last_used = Instant::now();
                self.store_acp_session(session).await;
            }
            Outcome::Settled(Err(error)) => {
                return Err(anyhow!(error).context("ACP session/prompt failed"));
            }
            Outcome::Cancelled => {
                return Err(anyhow!("ACP turn cancelled before the harness settled"));
            }
        }

        if projection.text.trim().is_empty() {
            return Ok(AcpTurnResult {
                text: "ACP harness completed without textual output".into(),
                transcript_entry_id,
            });
        }
        Ok(AcpTurnResult {
            text: projection.text,
            transcript_entry_id,
        })
    }

    /// Append the ACP turn evidence to the agent ledger.
    async fn persist_acp_evidence(
        &self,
        agent_id: &str,
        projection: &AcpTurnProjection,
        stop_reason: &str,
        permission_decisions: Vec<Value>,
        session_id: &str,
    ) -> Result<String> {
        let entry = crate::types::TranscriptEntry {
            stop_reason: Some(stop_reason.to_string()),
            // `usage_update` reports session context occupancy, never a
            // per-turn token delta, so the turn counters stay absent.
            input_tokens: None,
            output_tokens: None,
            ..crate::types::TranscriptEntry::new(
                agent_id.to_string(),
                crate::types::TranscriptEntryKind::AssistantRound,
                Some(1),
                None,
                json!({
                    "driver": "acp",
                    "session_id": session_id,
                    "blocks": projection.blocks,
                    "usage": projection.usage,
                    "evidence": projection.evidence,
                    "permission_decisions": permission_decisions,
                }),
            )
        };
        let entry_id = entry.id.clone();
        self.persist_transcript_evidence(&entry).await?;
        Ok(entry_id)
    }

    /// Binding for an agent whose inner loop is owned by an ACP harness.
    pub(crate) async fn acp_driver_binding(&self) -> Result<Option<AcpDriverBinding>> {
        let state = self.agent_state().await?;
        if state.execution_driver.is_native() {
            return Ok(None);
        }
        let config = self.inner.config_snapshot.load().acp.clone();
        let profile = state
            .execution_driver
            .harness_profile
            .clone()
            .or(config.default_harness_profile)
            .unwrap_or_else(|| holon_acp::antigravity::NAME.to_string());
        Ok(Some(AcpDriverBinding {
            profile,
            role: state.execution_driver.role,
        }))
    }

    /// Persist an explicit execution-driver selection on this agent.
    pub(crate) async fn apply_execution_driver(
        &self,
        driver: crate::types::AgentExecutionDriver,
    ) -> Result<()> {
        self.update_agent_state(|state| {
            state.execution_driver = driver;
            Ok(())
        })
        .await?;
        Ok(())
    }
}

/// How one ACP turn ended.
enum Outcome {
    Settled(Result<holon_acp::PromptResult, holon_acp::AcpError>),
    Cancelled,
}

pub(crate) struct AcpTurnResult {
    pub(crate) text: String,
    pub(crate) transcript_entry_id: String,
}

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
mod acp_driver_tests {
    use super::*;
    use holon_acp::protocol::{ContentBlock, PlanEntry, UsageCost};
    use holon_acp::PermissionOption;

    fn execution(root: &std::path::Path, writable: bool) -> EffectiveExecution {
        let access = if writable {
            crate::system::WorkspaceAccessMode::ExclusiveWrite
        } else {
            crate::system::WorkspaceAccessMode::SharedRead
        };
        let view = crate::system::WorkspaceView::new(
            Some("ws-acp".into()),
            root.to_path_buf(),
            root.to_path_buf(),
            root.to_path_buf(),
            None,
            Some(access),
            crate::system::WorkspaceProjectionKind::CanonicalRoot,
            None,
        )
        .expect("workspace view");
        EffectiveExecution {
            profile: crate::system::ExecutionProfile::default(),
            workspace: view,
            scope: crate::system::ExecutionScopeKind::AgentTurn,
            attached_workspaces: Vec::new(),
        }
    }

    #[test]
    fn projects_thought_tool_and_session_usage_without_token_deltas() {
        let projection = project_updates(&[
            SessionUpdate::AgentThoughtChunk {
                content: ContentBlock::text("planning"),
            },
            SessionUpdate::AgentMessageChunk {
                content: ContentBlock::text("done"),
            },
            SessionUpdate::ToolCall {
                tool_call: ToolCall {
                    tool_call_id: "call-1".into(),
                    kind: Some("read".into()),
                    raw_input: Some(json!({"path": "src/lib.rs"})),
                    ..ToolCall::default()
                },
            },
            SessionUpdate::UsageUpdate {
                used: 12,
                size: 100,
                cost: Some(UsageCost {
                    amount: 0.01,
                    currency: "USD".into(),
                }),
            },
            SessionUpdate::Plan {
                entries: vec![PlanEntry::default()],
            },
        ]);
        assert_eq!(projection.text, "done");
        assert!(matches!(
            projection.blocks[0],
            ModelBlock::ReasoningText { .. }
        ));
        assert!(matches!(projection.blocks[1], ModelBlock::Text { .. }));
        assert!(matches!(
            projection.blocks[2],
            ModelBlock::ProviderToolUse { .. }
        ));
        let usage = projection.usage.expect("usage");
        assert_eq!(usage["scope"], "acp_session_context_window");
        assert_eq!(usage["used"], 12);
        assert!(projection
            .evidence
            .iter()
            .any(|item| item["kind"] == "plan"));
    }

    #[test]
    fn permission_gate_rejects_paths_outside_the_bound_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = AcpPermissionGate::new(&execution(dir.path(), true));
        let outside = gate.decide(&PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({
                "toolCallId": "call-1",
                "kind": "read",
                "rawInput": {"path": "/etc/passwd"}
            }),
            options: vec![
                PermissionOption {
                    option_id: "allow_once".into(),
                    name: "Allow once".into(),
                    kind: None,
                },
                PermissionOption {
                    option_id: "reject_once".into(),
                    name: "Reject".into(),
                    kind: None,
                },
            ],
        });
        assert!(
            matches!(outside, PermissionOutcome::Selected { option_id } if option_id == "reject_once")
        );
        assert_eq!(
            gate.recorded()[0]["reason"],
            "path_outside_bound_execution_root"
        );

        let traversal = format!("{}/../etc/passwd", dir.path().display());
        let rejected_traversal = gate.decide(&PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({
                "toolCallId": "call-traversal",
                "kind": "read",
                "rawInput": {"path": traversal}
            }),
            options: vec![PermissionOption {
                option_id: "reject_once".into(),
                name: "Reject".into(),
                kind: None,
            }],
        });
        assert!(
            matches!(rejected_traversal, PermissionOutcome::Selected { option_id } if option_id == "reject_once")
        );
        assert_eq!(
            gate.recorded()[1]["reason"],
            "path_outside_bound_execution_root"
        );

        let inside = dir.path().join("src/lib.rs");
        let allowed = gate.decide(&PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({
                "toolCallId": "call-2",
                "kind": "read",
                "rawInput": {"path": inside}
            }),
            options: vec![PermissionOption {
                option_id: "allow_once".into(),
                name: "Allow once".into(),
                kind: None,
            }],
        });
        assert!(
            matches!(allowed, PermissionOutcome::Selected { option_id } if option_id == "allow_once")
        );
    }

    #[test]
    fn read_only_scope_rejects_write_kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = AcpPermissionGate::new(&execution(dir.path(), false));
        let outcome = gate.decide(&PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({"toolCallId": "call-3", "kind": "edit"}),
            options: vec![PermissionOption {
                option_id: "reject_once".into(),
                name: "Reject".into(),
                kind: None,
            }],
        });
        assert!(matches!(
            outcome,
            PermissionOutcome::Selected { .. } | PermissionOutcome::Cancelled
        ));
        assert_eq!(gate.recorded()[0]["reason"], "bound_workspace_is_read_only");
    }

    #[test]
    fn permission_gate_does_not_treat_persistent_allow_as_allow_once() {
        let request = PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({"toolCallId": "call-4", "kind": "read"}),
            options: vec![
                PermissionOption {
                    option_id: "allow_session".into(),
                    name: "Allow for session".into(),
                    kind: None,
                },
                PermissionOption {
                    option_id: "reject_once".into(),
                    name: "Reject".into(),
                    kind: None,
                },
            ],
        };
        assert_eq!(allow_once_option(&request), None);
    }

    #[test]
    fn execution_binding_is_what_a_resumed_session_must_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exec = execution(dir.path(), true);
        let (root, dirs) = execution_binding(&exec);
        assert_eq!(root, dir.path().to_string_lossy());
        assert!(binding_matches(
            "antigravity",
            &root,
            &dirs,
            "antigravity",
            &root,
            &dirs
        ));
    }

    #[test]
    fn resume_binding_rejects_a_different_profile_root_or_directory_set() {
        let dirs = vec!["/srv/shared".to_string()];
        assert!(binding_matches(
            "agy", "/root", &dirs, "agy", "/root", &dirs
        ));
        assert!(!binding_matches(
            "agy", "/root", &dirs, "other", "/root", &dirs
        ));
        assert!(!binding_matches(
            "agy", "/root", &dirs, "agy", "/root2", &dirs
        ));
        assert!(!binding_matches("agy", "/root", &dirs, "agy", "/root", &[]));
    }

    #[test]
    fn recovery_handoff_only_injects_a_summary_when_one_is_available() {
        assert_eq!(
            recovery_handoff_prompt("next step", Some("planner"), None),
            handoff_prompt("next step", Some("planner"))
        );
        assert_eq!(
            recovery_handoff_prompt("next step", None, Some("")),
            handoff_prompt("next step", None)
        );
        let with_summary = recovery_handoff_prompt("next step", None, Some("last words"));
        assert!(with_summary
            .starts_with("Bounded handoff from the previous ACP session:\nlast words\n\n"));
        assert!(with_summary.ends_with(&handoff_prompt("next step", None)));
    }

    #[test]
    fn acp_session_state_round_trips_without_binding_agent_identity() {
        let state = crate::types::AcpSessionState {
            acp_profile: "antigravity".into(),
            resume_session_id: "sess-1".into(),
            bound_execution_root: "/root".into(),
            bound_additional_dirs: vec!["/srv/shared".into()],
        };
        let encoded = serde_json::to_value(&state).expect("encode");
        let decoded: crate::types::AcpSessionState =
            serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, state);

        // Older state payloads predate the additional-directory binding.
        let legacy = serde_json::json!({
            "acp_profile": "antigravity",
            "resume_session_id": "sess-1",
            "bound_execution_root": "/root",
        });
        let decoded_legacy: crate::types::AcpSessionState =
            serde_json::from_value(legacy).expect("decode legacy");
        assert!(decoded_legacy.bound_additional_dirs.is_empty());
    }

    // --- Continuity: hot reuse, idle retirement, and degraded rotation. ---

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use holon_acp::jsonrpc::{Frame, RpcId};
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use crate::context::ContextConfig;
    use crate::provider::StubProvider;
    use crate::runtime::RuntimeHandle;
    use crate::system::process::ProcessOutput;
    use crate::system::RunningProcessExitStatus;

    /// How the mock peer answers session continuation calls.
    #[derive(Clone, Copy)]
    struct TestPeerConfig {
        supports_resume: bool,
        supports_load: bool,
        resume_missing: bool,
    }

    /// Session ids are unique per mock connection so a rotated session is
    /// distinguishable from the retired one, matching real peers.
    static TEST_PEER_SESSION_SEQ: AtomicUsize = AtomicUsize::new(0);

    #[derive(Default)]
    struct TestPeerLogState {
        handshakes: usize,
        methods: Vec<String>,
        prompts: Vec<(String, String)>,
        new_sessions: Vec<String>,
        resumed_sessions: Vec<String>,
    }

    /// What the mock peer observed across every connection it served.
    #[derive(Default)]
    struct TestPeerLog {
        state: std::sync::Mutex<TestPeerLogState>,
    }

    impl TestPeerLog {
        fn handshakes(&self) -> usize {
            self.state.lock().unwrap().handshakes
        }

        fn method_count(&self, method: &str) -> usize {
            self.state
                .lock()
                .unwrap()
                .methods
                .iter()
                .filter(|seen| *seen == method)
                .count()
        }

        fn prompts(&self) -> Vec<(String, String)> {
            self.state.lock().unwrap().prompts.clone()
        }

        fn new_sessions(&self) -> Vec<String> {
            self.state.lock().unwrap().new_sessions.clone()
        }

        fn resumed_sessions(&self) -> Vec<String> {
            self.state.lock().unwrap().resumed_sessions.clone()
        }
    }

    /// The process-group proof a test harness hands to the reaper.
    #[derive(Default)]
    struct TestProcessGroup {
        empty: AtomicBool,
        graceful_stops: AtomicUsize,
    }

    impl RegisteredProcessGroupProbe for TestProcessGroup {
        fn observe(&self) -> RegisteredProcessGroupQuiescence {
            if self.empty.load(Ordering::SeqCst) {
                RegisteredProcessGroupQuiescence::Empty
            } else {
                RegisteredProcessGroupQuiescence::NonEmpty
            }
        }
    }

    /// Stand-in for the spawned `agy acp` process.
    struct TestHarnessProcess {
        group: Arc<TestProcessGroup>,
        /// Simulates a resident harness that exited on its own.
        exited: bool,
    }

    #[async_trait::async_trait]
    impl RunningProcess for TestHarnessProcess {
        fn id(&self) -> String {
            "test-acp-harness".into()
        }

        fn take_stdout(&mut self) -> Option<Box<dyn ProcessOutput>> {
            None
        }

        fn take_stderr(&mut self) -> Option<Box<dyn ProcessOutput>> {
            None
        }

        async fn write_stdin(&mut self, _data: &[u8]) -> Result<()> {
            Ok(())
        }

        async fn wait(&mut self) -> Result<RunningProcessExitStatus> {
            Ok(RunningProcessExitStatus::new(Some(0), None))
        }

        async fn try_status(&mut self) -> Result<Option<RunningProcessExitStatus>> {
            Ok(self
                .exited
                .then(|| RunningProcessExitStatus::new(Some(0), None)))
        }

        async fn stop(&mut self, signal: StopSignal) -> Result<()> {
            if signal == StopSignal::Graceful {
                self.group.graceful_stops.fetch_add(1, Ordering::SeqCst);
            }
            self.group.empty.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn acp_continuity_runtime() -> (tempfile::TempDir, tempfile::TempDir, RuntimeHandle) {
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

    async fn install_test_peer(
        runtime: &RuntimeHandle,
        config: TestPeerConfig,
    ) -> Arc<TestPeerLog> {
        install_test_peer_with(runtime, config, 0).await
    }

    /// Install a mock peer whose first `exited_processes` harness spawns report
    /// a dead process, simulating a resident harness that exited on its own.
    async fn install_test_peer_with(
        runtime: &RuntimeHandle,
        config: TestPeerConfig,
        exited_processes: usize,
    ) -> Arc<TestPeerLog> {
        let log = Arc::new(TestPeerLog::default());
        let factory_log = Arc::clone(&log);
        let config = Arc::new(config);
        let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        runtime
            .install_acp_test_harness(Arc::new(move |handler| {
                let (client_io, server_io) = tokio::io::duplex(64 * 1024);
                tokio::spawn(run_test_peer(
                    server_io,
                    Arc::clone(&config),
                    Arc::clone(&factory_log),
                ));
                let client = Arc::new(AcpClient::new(client_io, handler));
                let group = Arc::new(TestProcessGroup::default());
                let spawn_index = spawns.fetch_add(1, Ordering::SeqCst);
                let process: Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>> =
                    Arc::new(tokio::sync::Mutex::new(Box::new(TestHarnessProcess {
                        group: Arc::clone(&group),
                        exited: spawn_index < exited_processes,
                    })));
                AcpTestHarness {
                    client,
                    process,
                    group: Some(group),
                }
            }))
            .await;
        log
    }

    async fn write_peer_frame(writer: &mut (impl AsyncWriteExt + Unpin), frame: Frame) {
        let mut line = frame.to_line().expect("frame line");
        line.push('\n');
        writer.write_all(line.as_bytes()).await.unwrap();
        writer.flush().await.unwrap();
    }

    fn param_session_id(params: &Option<Value>) -> String {
        params
            .as_ref()
            .and_then(|params| params.get("sessionId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn prompt_text(params: &Option<Value>) -> String {
        params
            .as_ref()
            .and_then(|params| params.get("prompt"))
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    }

    async fn run_test_peer(
        stream: tokio::io::DuplexStream,
        config: Arc<TestPeerConfig>,
        log: Arc<TestPeerLog>,
    ) {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut lines = BufReader::new(read_half).lines();
        while let Some(line) = lines.next_line().await.expect("mock peer read") {
            if line.trim().is_empty() {
                continue;
            }
            let frame = Frame::parse(&line).expect("mock peer frame");
            let Frame::Request { id, method, params } = frame else {
                continue;
            };
            {
                log.state.lock().unwrap().methods.push(method.clone());
            }
            match method.as_str() {
                "initialize" => {
                    log.state.lock().unwrap().handshakes += 1;
                    let capabilities = json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": config.supports_load,
                            "sessionCapabilities": if config.supports_resume {
                                json!({ "resume": {} })
                            } else {
                                json!({})
                            },
                        },
                        "authMethods": [],
                    });
                    write_peer_frame(
                        &mut write_half,
                        Frame::Response {
                            id,
                            result: capabilities,
                        },
                    )
                    .await;
                }
                "session/new" => {
                    let session_id = {
                        let mut state = log.state.lock().unwrap();
                        let next = TEST_PEER_SESSION_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
                        let session_id = format!("s{next}");
                        state.new_sessions.push(session_id.clone());
                        session_id
                    };
                    write_peer_frame(
                        &mut write_half,
                        Frame::Response {
                            id,
                            result: json!({ "sessionId": session_id }),
                        },
                    )
                    .await;
                }
                "session/resume" => {
                    let requested = param_session_id(&params);
                    log.state
                        .lock()
                        .unwrap()
                        .resumed_sessions
                        .push(requested.clone());
                    let reply = if config.resume_missing {
                        Frame::Error {
                            id,
                            error: holon_acp::RpcError {
                                code: -32002,
                                message: "session not found".into(),
                                data: None,
                            },
                        }
                    } else {
                        Frame::Response {
                            id,
                            result: json!({ "sessionId": requested }),
                        }
                    };
                    write_peer_frame(&mut write_half, reply).await;
                }
                "session/load" => {
                    let requested = param_session_id(&params);
                    log.state
                        .lock()
                        .unwrap()
                        .resumed_sessions
                        .push(requested.clone());
                    // Replay history the client must drain without re-writing
                    // it to the Holon ledger.
                    write_peer_frame(
                        &mut write_half,
                        Frame::Notification {
                            method: "session/update".into(),
                            params: Some(json!({
                                "sessionId": requested,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": { "type": "text", "text": "replayed" },
                                },
                            })),
                        },
                    )
                    .await;
                    write_peer_frame(
                        &mut write_half,
                        Frame::Response {
                            id,
                            result: json!({ "sessionId": requested }),
                        },
                    )
                    .await;
                }
                "session/prompt" => {
                    let session_id = param_session_id(&params);
                    let text = prompt_text(&params);
                    let round = {
                        let mut state = log.state.lock().unwrap();
                        state.prompts.push((session_id.clone(), text));
                        state.prompts.len()
                    };
                    write_peer_frame(
                        &mut write_half,
                        Frame::Notification {
                            method: "session/update".into(),
                            params: Some(json!({
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {
                                        "type": "text",
                                        "text": format!("round {round}"),
                                    },
                                },
                            })),
                        },
                    )
                    .await;
                    write_peer_frame(
                        &mut write_half,
                        Frame::Response {
                            id,
                            result: json!({ "stopReason": "end_turn" }),
                        },
                    )
                    .await;
                }
                other => {
                    write_peer_frame(
                        &mut write_half,
                        Frame::Error {
                            id: RpcId::Number(0),
                            error: holon_acp::RpcError {
                                code: -32601,
                                message: format!("unsupported method {other}"),
                                data: None,
                            },
                        },
                    )
                    .await;
                }
            }
        }
    }

    fn antigravity_binding() -> AcpDriverBinding {
        AcpDriverBinding {
            profile: holon_acp::antigravity::NAME.into(),
            role: None,
        }
    }

    async fn turn(runtime: &RuntimeHandle, execution: &EffectiveExecution, prompt: &str) -> String {
        runtime
            .run_acp_turn(
                "default",
                prompt,
                execution,
                &antigravity_binding(),
                CancellationToken::new(),
            )
            .await
            .expect("ACP turn")
            .text
    }

    #[tokio::test]
    async fn resident_session_reuses_one_peer_across_turns_without_replaying_history() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let peer = install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: false,
            },
        )
        .await;
        let root = tempdir().unwrap();
        let execution = execution(root.path(), true);

        turn(&runtime, &execution, "first request").await;
        turn(&runtime, &execution, "second request").await;

        assert_eq!(peer.handshakes(), 1, "the resident peer is handshaken once");
        assert_eq!(peer.method_count("session/new"), 1);
        assert_eq!(peer.method_count("session/prompt"), 2);
        let prompts = peer.prompts();
        assert_eq!(prompts[0].0, prompts[1].0, "both rounds share one session");
        assert!(prompts[1].1.contains("second request"));
        assert!(
            !prompts[1].1.contains("first request"),
            "the peer must not receive replayed ledger history: {}",
            prompts[1].1
        );
        assert!(
            !prompts[1].1.contains("<runtime_context>"),
            "the internal context envelope must never reach the peer: {}",
            prompts[1].1
        );
    }

    #[tokio::test]
    async fn idle_retirement_resumes_the_persisted_session_on_a_fresh_peer() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let peer = install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: false,
            },
        )
        .await;
        let root = tempdir().unwrap();
        let execution = execution(root.path(), true);

        turn(&runtime, &execution, "first request").await;
        let persisted = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state is persisted");
        runtime.retire_acp_session("idle-retirement").await;

        turn(&runtime, &execution, "second request").await;

        assert_eq!(peer.handshakes(), 2, "retirement forces a new handshake");
        assert_eq!(
            peer.method_count("session/new"),
            1,
            "no session is recreated"
        );
        assert_eq!(peer.method_count("session/resume"), 1);
        assert_eq!(
            peer.resumed_sessions(),
            vec![persisted.resume_session_id.clone()]
        );
        assert_eq!(peer.prompts()[1].0, persisted.resume_session_id);
    }

    #[tokio::test]
    async fn missing_upstream_session_rotates_to_session_new_with_a_bounded_handoff() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let peer = install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: false,
            },
        )
        .await;
        let root = tempdir().unwrap();
        let execution = execution(root.path(), true);

        turn(&runtime, &execution, "first request").await;
        let first = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state is persisted");
        runtime.retire_acp_session("idle-retirement").await;
        runtime
            .update_agent_state(|state| {
                state.last_turn_terminal = Some(crate::types::TurnTerminalRecord {
                    turn_id: "turn-prior".into(),
                    turn_index: 1,
                    kind: crate::types::TurnTerminalKind::Completed,
                    reason: None,
                    last_assistant_message: Some("prior delegated summary".into()),
                    no_brief_reason: None,
                    checkpoint: None,
                    completed_at: chrono::Utc::now(),
                    duration_ms: 1,
                });
                Ok(())
            })
            .await
            .unwrap();
        // The next peer no longer knows the persisted session.
        let resumed = install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: true,
            },
        )
        .await;

        turn(&runtime, &execution, "second request").await;

        assert_eq!(peer.method_count("session/new"), 1);
        assert_eq!(resumed.method_count("session/resume"), 1);
        assert_eq!(resumed.method_count("session/new"), 1);
        assert_eq!(
            resumed.resumed_sessions(),
            vec![first.resume_session_id.clone()]
        );
        let rotated = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("rotated session state is persisted");
        assert_eq!(
            resumed.new_sessions(),
            vec![rotated.resume_session_id.clone()]
        );
        assert_ne!(rotated.resume_session_id, first.resume_session_id);
        let prompt = resumed.prompts().pop().expect("a prompt was sent").1;
        assert!(
            prompt.contains("Bounded handoff from the previous ACP session:"),
            "the degraded path injects the bounded handoff: {prompt}"
        );
        assert!(prompt.contains("prior delegated summary"));
        assert!(prompt.contains("second request"));

        let connection = runtime.runtime_db().connection().unwrap();
        let rotations: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM audit_events WHERE kind = 'acp_session_rotated'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rotations, 1, "the rotation is audited");
    }

    #[tokio::test]
    async fn permission_downgrade_rebuilds_the_resident_and_resumes_the_session() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let peer = install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: false,
            },
        )
        .await;
        let root = tempdir().unwrap();
        let writable = execution(root.path(), true);
        let read_only = execution(root.path(), false);

        turn(&runtime, &writable, "first request").await;
        let persisted = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state is persisted");
        turn(&runtime, &read_only, "second request").await;

        assert_eq!(
            peer.handshakes(),
            2,
            "a downgraded permission posture must not reuse the old gate"
        );
        assert_eq!(
            peer.method_count("session/new"),
            1,
            "the conversation continues instead of restarting"
        );
        assert_eq!(peer.method_count("session/resume"), 1);
        assert_eq!(peer.prompts()[1].0, persisted.resume_session_id);
    }

    #[tokio::test]
    async fn dead_resident_process_is_replaced_and_the_session_resumes() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let peer = install_test_peer_with(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: false,
            },
            1,
        )
        .await;
        let root = tempdir().unwrap();
        let execution = execution(root.path(), true);

        turn(&runtime, &execution, "first request").await;
        let persisted = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state is persisted");
        turn(&runtime, &execution, "second request").await;

        assert_eq!(peer.handshakes(), 2, "a dead resident is replaced");
        assert_eq!(peer.method_count("session/resume"), 1);
        assert_eq!(peer.prompts()[1].0, persisted.resume_session_id);
    }

    #[test]
    fn permission_gate_drains_recorded_decisions_between_turns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = AcpPermissionGate::new(&execution(dir.path(), true));
        let request = PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({"toolCallId": "call-1", "kind": "read"}),
            options: vec![PermissionOption {
                option_id: "allow_once".into(),
                name: "Allow once".into(),
                kind: None,
            }],
        };

        assert!(matches!(
            gate.decide(&request),
            PermissionOutcome::Selected { .. }
        ));
        assert!(matches!(
            gate.decide(&request),
            PermissionOutcome::Selected { .. }
        ));
        assert_eq!(gate.drain().len(), 2, "one turn sees only its decisions");
        assert!(
            gate.drain().is_empty(),
            "a resident gate must not replay earlier turns"
        );
    }
}
