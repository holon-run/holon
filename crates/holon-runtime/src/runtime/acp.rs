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
use std::time::Duration;

use anyhow::{anyhow, ensure, Context, Result};
use holon_acp::harness::{AcpHarnessSpec, LaunchRequest, SandboxMode};
use holon_acp::protocol::{
    ClientCapabilities, ContentBlock, InitializeParams, PromptParams, SessionNewParams,
    SessionUpdate, StopReason, ToolCall, ToolCallStatus, ToolCallUpdate,
};
use holon_acp::{
    AcpClient, AcpClientHandler, CancelHandle, ElicitationRequest, PermissionOutcome,
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
    let client = AcpClient::from_split(stdout, stdin, gate as Arc<dyn AcpClientHandler>);
    Ok(SpawnedHarness {
        client,
        process,
        group,
    })
}

/// A spawned harness process and its bridged ACP client.
struct SpawnedHarness {
    client: AcpClient,
    process: Arc<tokio::sync::Mutex<Box<dyn RunningProcess>>>,
    group: Option<Arc<dyn RegisteredProcessGroupProbe>>,
}

impl super::RuntimeHandle {
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
        let spawned = spawn_harness(self, &profile, execution, Arc::clone(&gate)).await?;
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
        let root = execution.workspace.execution_root();
        let additional_directories = execution
            .attached_workspaces
            .iter()
            .map(|(_, path)| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut session_params = SessionNewParams::new(root.to_string_lossy().into_owned())
            .with_additional_directories(additional_directories)
            .with_meta(session_meta(&profile));
        // The negotiated persona channel is preferred. When the peer does not
        // declare it, the role travels as ordinary handoff prose instead.
        session_params.system_prompt = negotiated_role(&profile, &handshake);
        let negotiated = session_params.system_prompt.clone();

        let session = match client.new_session(session_params).await {
            Ok(session) => session,
            Err(error) => {
                reap_process(&process, group.as_ref()).await;
                return Err(anyhow!(error).context("ACP session/new failed"));
            }
        };
        let control = AcpSessionControl {
            cancel: client.cancel_handle(session.session_id.clone()),
            process,
            group,
        };

        let handoff = handoff_prompt(prompt, negotiated.as_deref());
        let mut updates: Vec<SessionUpdate> = Vec::new();
        let outcome = {
            let prompt_future = client.prompt(
                PromptParams::text(session.session_id.clone(), handoff),
                |update| updates.push(update.clone()),
            );
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
        control.cancel_and_reap().await;
        let transcript_entry_id = self
            .persist_acp_evidence(
                agent_id,
                &projection,
                &stop_reason,
                gate.recorded(),
                session.session_id.as_str(),
            )
            .await?;

        match outcome {
            Outcome::Settled(Ok(_)) => {}
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
}
