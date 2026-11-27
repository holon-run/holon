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
    AcpClient, AcpClientHandler, AcpError, CancelHandle, ElicitationRequest, ElicitationResponse,
    PermissionOutcome, PermissionRequest,
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

/// How the driver treats permission and elicitation prompts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpPermissionMode {
    /// Never wait for an operator; apply the static gate and its fallbacks.
    Unattended,
    /// Surface the request through the control plane and wait, bounded by the
    /// configured interaction timeout.
    Interactive,
}

impl AcpPermissionMode {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("unattended") {
            "unattended" => Ok(Self::Unattended),
            "interactive" => Ok(Self::Interactive),
            other => Err(anyhow!(
                "unknown ACP permission mode `{other}`; expected `unattended` or `interactive`"
            )),
        }
    }

    fn is_interactive(self) -> bool {
        self == Self::Interactive
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Unattended => "unattended",
            Self::Interactive => "interactive",
        }
    }
}

/// A decision an operator can return for a pending permission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpPermissionDecisionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// What the gate is waiting for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AcpInteractionRequest {
    Permission {
        tool_call: Value,
        options: Vec<holon_acp::PermissionOption>,
        scope: AcpGrantScope,
    },
    Elicitation {
        message: String,
        requested_schema: Option<Value>,
        tool_call_id: Option<String>,
    },
}

impl AcpInteractionRequest {
    fn kind(&self) -> &'static str {
        match self {
            Self::Permission { .. } => "permission",
            Self::Elicitation { .. } => "elicitation",
        }
    }
}

/// The operator answer that resolves a pending interaction.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AcpInteractionResolution {
    Permission(AcpPermissionDecisionKind),
    Elicitation(ElicitationResponse),
}

/// Parse an operator-supplied interaction resolution body.
pub(crate) fn parse_interaction_resolution(body: &Value) -> Result<AcpInteractionResolution> {
    if let Some(decision) = body.get("decision").and_then(Value::as_str) {
        let decision = match decision {
            "allow_once" => AcpPermissionDecisionKind::AllowOnce,
            "allow_always" => AcpPermissionDecisionKind::AllowAlways,
            "reject_once" => AcpPermissionDecisionKind::RejectOnce,
            "reject_always" => AcpPermissionDecisionKind::RejectAlways,
            other => {
                return Err(anyhow!(
                    "unknown permission decision `{other}`; expected allow_once, allow_always, reject_once, or reject_always"
                ));
            }
        };
        return Ok(AcpInteractionResolution::Permission(decision));
    }
    if let Some(action) = body.get("action").and_then(Value::as_str) {
        let response = match action {
            "accept" => ElicitationResponse::Accept {
                content: body
                    .get("content")
                    .cloned()
                    .filter(|content| !content.is_null()),
            },
            "decline" => ElicitationResponse::Decline,
            "cancel" => ElicitationResponse::Cancel,
            other => {
                return Err(anyhow!(
                    "unknown elicitation action `{other}`; expected accept, decline, or cancel"
                ));
            }
        };
        return Ok(AcpInteractionResolution::Elicitation(response));
    }
    Err(anyhow!(
        "interaction resolution requires `decision` or `action`"
    ))
}

/// One pending interaction and the channel that delivers its answer.
#[derive(Debug)]
pub(crate) struct PendingAcpInteraction {
    pub(crate) id: String,
    pub(crate) agent_id: String,
    pub(crate) session_id: String,
    pub(crate) created_at: chrono::DateTime<chrono::Utc>,
    pub(crate) request: AcpInteractionRequest,
    pub(crate) responder: tokio::sync::oneshot::Sender<AcpInteractionResolution>,
}

impl PendingAcpInteraction {
    /// Operator-facing view. Never carries the response channel.
    pub(crate) fn to_value(&self) -> Value {
        let mut value = json!({
            "interaction_id": self.id,
            "agent_id": self.agent_id,
            "session_id": self.session_id,
            "kind": self.request.kind(),
            "created_at": self.created_at,
        });
        if let AcpInteractionRequest::Permission {
            tool_call,
            options,
            scope,
        } = &self.request
        {
            value["tool_call"] = tool_call.clone();
            value["options"] = serde_json::to_value(options).unwrap_or(Value::Null);
            value["scope"] = scope.to_value();
        }
        if let AcpInteractionRequest::Elicitation {
            message,
            requested_schema,
            tool_call_id,
        } = &self.request
        {
            value["message"] = json!(message);
            value["requested_schema"] = requested_schema.clone().unwrap_or(Value::Null);
            value["tool_call_id"] = json!(tool_call_id);
        }
        value
    }
}

/// Why a pending interaction left the registry without operator input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpSettledReason {
    /// An operator answered it.
    Resolved,
    /// The bounded wait elapsed before an answer arrived.
    TimedOut,
    /// The asking turn was cancelled or its future was dropped.
    Cancelled,
}

impl AcpSettledReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A recently settled interaction, retained briefly so a late operator answer
/// can be explained instead of reported as nonexistent.
#[derive(Clone, Debug)]
pub(crate) struct AcpSettledInteraction {
    pub(crate) reason: AcpSettledReason,
    pub(crate) settled_at: chrono::DateTime<chrono::Utc>,
}

/// How a resolution attempt ended, so the control plane can distinguish a
/// settled interaction from one that never existed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpInteractionOutcome {
    /// The pending interaction existed and was answered.
    Resolved,
    /// The interaction already settled (resolved, timed out, or cancelled).
    AlreadySettled(&'static str),
    /// No such interaction was ever registered, or it aged out of the registry.
    Unknown,
}

/// How long a settled interaction stays available to explain a late answer.
const ACP_SETTLED_TTL: Duration = Duration::from_secs(600);
/// Upper bound on retained settled interactions per runtime.
const ACP_SETTLED_CAP: usize = 128;

fn insert_settled_interaction(
    settled: &mut BTreeMap<String, AcpSettledInteraction>,
    id: String,
    reason: AcpSettledReason,
) {
    let now = chrono::Utc::now();
    let cutoff = now
        - chrono::Duration::from_std(ACP_SETTLED_TTL)
            .unwrap_or_else(|_| chrono::Duration::seconds(600));
    settled.retain(|_, entry| entry.settled_at >= cutoff);
    settled.insert(
        id,
        AcpSettledInteraction {
            reason,
            settled_at: now,
        },
    );
    while settled.len() > ACP_SETTLED_CAP {
        let oldest = settled
            .iter()
            .min_by_key(|(_, entry)| entry.settled_at)
            .map(|(key, _)| key.clone());
        match oldest {
            Some(key) => {
                settled.remove(&key);
            }
            None => break,
        }
    }
}

/// Scope key for one `allow_always` grant.
///
/// The key is deliberately conservative: the same normalized tool kind and the
/// exact set of claimed paths must repeat inside the same session and trust
/// domain. A request without a path claim never widens a path grant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AcpGrantScope {
    pub(crate) tool_kind: Option<String>,
    pub(crate) path_scope: Option<String>,
}

impl AcpGrantScope {
    fn to_value(&self) -> Value {
        json!({
            "tool_kind": self.tool_kind,
            "path_scope": self.path_scope,
        })
    }

    fn matches_grant(&self, grant: &crate::types::AcpSessionGrant, trust_domain: &str) -> bool {
        grant.trust_domain == trust_domain
            && grant.tool_kind == self.tool_kind
            && grant.path_scope == self.path_scope
    }

    fn to_grant(&self, trust_domain: &str) -> crate::types::AcpSessionGrant {
        crate::types::AcpSessionGrant {
            tool_kind: self.tool_kind.clone(),
            path_scope: self.path_scope.clone(),
            trust_domain: trust_domain.to_string(),
        }
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
    mode: AcpPermissionMode,
    trust_domain: String,
    grants: Mutex<Vec<crate::types::AcpSessionGrant>>,
    decisions: Mutex<Vec<AcpPermissionDecision>>,
    interaction: Option<Arc<dyn AcpInteractionChannel>>,
}

impl AcpPermissionGate {
    fn new(
        execution: &EffectiveExecution,
        mode: AcpPermissionMode,
        interaction: Option<Arc<dyn AcpInteractionChannel>>,
    ) -> Self {
        let mut roots = vec![execution.workspace.execution_root().to_path_buf()];
        roots.extend(
            execution
                .attached_workspaces
                .iter()
                .map(|(_, path)| path.clone()),
        );
        let trust_domain = roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            roots,
            writable: matches!(
                execution.workspace.access_mode(),
                Some(crate::system::WorkspaceAccessMode::ExclusiveWrite)
            ),
            process_execution_exposed: execution.profile.process_execution_exposed,
            mode,
            trust_domain,
            grants: Mutex::new(Vec::new()),
            decisions: Mutex::new(Vec::new()),
            interaction,
        }
    }

    /// Load persisted session grants. Only the same logical session may carry
    /// them: a rebuilt or newly created session starts with an empty set.
    fn load_grants(&self, grants: &[crate::types::AcpSessionGrant]) {
        if let Ok(mut current) = self.grants.lock() {
            *current = grants.to_vec();
        }
    }

    fn grants(&self) -> Vec<crate::types::AcpSessionGrant> {
        self.grants
            .lock()
            .map(|grants| grants.clone())
            .unwrap_or_default()
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
    ///
    /// Policy rejection always wins. Only a request that passes policy may be
    /// answered by a cached session grant or by an operator decision.
    async fn decide(&self, request: &PermissionRequest) -> PermissionOutcome {
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

        let Some(allow_option) = allow_once_option(request) else {
            self.record(AcpPermissionDecision {
                outcome: "reject",
                reason: "no_allow_option_in_request".to_string(),
                tool_call_id,
                kind,
                path: None,
            });
            return PermissionOutcome::Cancelled;
        };

        let scope = self.request_scope(request);
        if self.grant_matches(&scope) {
            self.record(AcpPermissionDecision {
                outcome: "allow_always",
                reason: "session_grant_matches".to_string(),
                tool_call_id,
                kind,
                path: scope.path_scope.clone(),
            });
            return PermissionOutcome::Selected {
                option_id: allow_option,
            };
        }

        if self.mode.is_interactive() {
            if let Some(interaction) = self.interaction.as_ref() {
                match interaction
                    .ask_permission(&request.session_id, request, scope.clone())
                    .await
                {
                    Some(AcpPermissionDecisionKind::AllowOnce) => {
                        self.record(AcpPermissionDecision {
                            outcome: "allow_once",
                            reason: "operator_approved_once".to_string(),
                            tool_call_id,
                            kind,
                            path: None,
                        });
                        return PermissionOutcome::Selected {
                            option_id: allow_option,
                        };
                    }
                    Some(AcpPermissionDecisionKind::AllowAlways) => {
                        self.record(AcpPermissionDecision {
                            outcome: "allow_always",
                            reason: "operator_approved_for_session".to_string(),
                            tool_call_id,
                            kind,
                            path: scope.path_scope.clone(),
                        });
                        self.record_grant(scope);
                        return PermissionOutcome::Selected {
                            option_id: allow_always_option(request).unwrap_or(allow_option),
                        };
                    }
                    Some(AcpPermissionDecisionKind::RejectOnce) => {
                        return self.reject(
                            request,
                            AcpPermissionDecision {
                                outcome: "reject",
                                reason: "operator_rejected".to_string(),
                                tool_call_id,
                                kind,
                                path: None,
                            },
                        );
                    }
                    Some(AcpPermissionDecisionKind::RejectAlways) => {
                        return self.reject(
                            request,
                            AcpPermissionDecision {
                                outcome: "reject",
                                reason: "operator_rejected_always".to_string(),
                                tool_call_id,
                                kind,
                                path: None,
                            },
                        );
                    }
                    None => {
                        return self.reject(
                            request,
                            AcpPermissionDecision {
                                outcome: "reject",
                                reason: "interaction_timeout_or_cancelled".to_string(),
                                tool_call_id,
                                kind,
                                path: None,
                            },
                        );
                    }
                }
            }
        }

        self.record(AcpPermissionDecision {
            outcome: "allow_once",
            reason: "request_stays_inside_the_bound_execution_scope".to_string(),
            tool_call_id,
            kind,
            path: None,
        });
        PermissionOutcome::Selected {
            option_id: allow_option,
        }
    }

    fn request_scope(&self, request: &PermissionRequest) -> AcpGrantScope {
        let kind = string_field(&request.tool_call, "kind")
            .map(|kind| kind.to_ascii_lowercase())
            .filter(|kind| !kind.is_empty());
        let mut paths = candidate_paths(&request.tool_call);
        paths.sort();
        paths.dedup();
        AcpGrantScope {
            tool_kind: kind,
            path_scope: (!paths.is_empty()).then(|| paths.join("\n")),
        }
    }

    fn grant_matches(&self, scope: &AcpGrantScope) -> bool {
        self.grants
            .lock()
            .map(|grants| {
                grants
                    .iter()
                    .any(|grant| scope.matches_grant(grant, &self.trust_domain))
            })
            .unwrap_or(false)
    }

    fn record_grant(&self, scope: AcpGrantScope) {
        if let Ok(mut grants) = self.grants.lock() {
            let grant = scope.to_grant(&self.trust_domain);
            if !grants.iter().any(|existing| *existing == grant) {
                grants.push(grant);
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
        self.decide(&request).await
    }

    async fn create_elicitation(
        &self,
        request: ElicitationRequest,
    ) -> Result<holon_acp::ElicitationResponse, holon_acp::AcpError> {
        if !request.is_form() {
            return Err(holon_acp::AcpError::UnsupportedCapability {
                capability: "elicitation.url".to_string(),
            });
        }
        if !self.mode.is_interactive() {
            return Err(holon_acp::AcpError::UnsupportedCapability {
                capability: "elicitation".to_string(),
            });
        }
        let Some(interaction) = self.interaction.as_ref() else {
            return Err(holon_acp::AcpError::UnsupportedCapability {
                capability: "elicitation".to_string(),
            });
        };
        match interaction
            .ask_elicitation(&request.session_id, &request)
            .await
        {
            Some(AcpInteractionResolution::Elicitation(response)) => Ok(response),
            Some(AcpInteractionResolution::Permission(_)) => Err(holon_acp::AcpError::Protocol(
                "permission decision resolved an elicitation request".to_string(),
            )),
            // Timeout and cancellation both fall back to a structured decline,
            // never to an unbounded wait.
            None => Ok(holon_acp::ElicitationResponse::Decline),
        }
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

/// The request option that grants the same approval for the rest of the
/// session. The gate only records a session grant when an operator explicitly
/// chose this option.
fn allow_always_option(request: &PermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| {
            let id = option.option_id.to_ascii_lowercase();
            let name = option.name.to_ascii_lowercase();
            (id.contains("allow")
                || id.contains("approve")
                || name.contains("allow")
                || name.contains("approve"))
                && (id.contains("always")
                    || id.contains("session")
                    || id.contains("persist")
                    || name.contains("always")
                    || name.contains("session"))
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

/// Operator-facing channel for one interactive ACP turn.
#[async_trait::async_trait]
pub(crate) trait AcpInteractionChannel: Send + Sync {
    async fn ask_permission(
        &self,
        session_id: &str,
        request: &PermissionRequest,
        scope: AcpGrantScope,
    ) -> Option<AcpPermissionDecisionKind>;

    async fn ask_elicitation(
        &self,
        session_id: &str,
        request: &ElicitationRequest,
    ) -> Option<AcpInteractionResolution>;
}

/// Bridges ACP reverse requests to the runtime's pending-interaction registry.
///
pub(crate) struct AcpInteractionBroker {
    runtime: std::sync::Weak<super::RuntimeInner>,
    agent_id: String,
    timeout: Duration,
}

impl AcpInteractionBroker {
    pub(crate) fn new(
        runtime: std::sync::Weak<super::RuntimeInner>,
        agent_id: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            runtime,
            agent_id: agent_id.into(),
            timeout,
        }
    }

    fn runtime(&self) -> Option<super::RuntimeHandle> {
        self.runtime
            .upgrade()
            .map(|inner| super::RuntimeHandle { inner })
    }

    async fn broker_permission(
        &self,
        session_id: &str,
        request: &PermissionRequest,
        scope: AcpGrantScope,
    ) -> Option<AcpPermissionDecisionKind> {
        match self
            .ask(
                session_id,
                AcpInteractionRequest::Permission {
                    tool_call: request.tool_call.clone(),
                    options: request.options.clone(),
                    scope,
                },
            )
            .await
        {
            Some(AcpInteractionResolution::Permission(decision)) => Some(decision),
            Some(AcpInteractionResolution::Elicitation(_)) | None => None,
        }
    }

    async fn broker_elicitation(
        &self,
        session_id: &str,
        request: &ElicitationRequest,
    ) -> Option<AcpInteractionResolution> {
        self.ask(
            session_id,
            AcpInteractionRequest::Elicitation {
                message: request.message.clone(),
                requested_schema: request.requested_schema.clone(),
                tool_call_id: request.tool_call_id.clone(),
            },
        )
        .await
    }

    async fn ask(
        &self,
        session_id: &str,
        request: AcpInteractionRequest,
    ) -> Option<AcpInteractionResolution> {
        let runtime = self.runtime()?;
        let (responder, receiver) = tokio::sync::oneshot::channel();
        let id = crate::ids::runtime_id("acpint");
        let pending = PendingAcpInteraction {
            id: id.clone(),
            agent_id: self.agent_id.clone(),
            session_id: session_id.to_string(),
            created_at: chrono::Utc::now(),
            request,
            responder,
        };
        if let Err(error) = runtime.register_acp_interaction(pending).await {
            tracing::warn!(%error, "failed to register ACP interaction");
            return None;
        }
        // Keep the registry entry and the waiting future paired: if this turn
        // is cancelled and the future is dropped, the guard removes the entry.
        let guard = PendingInteractionGuard::new(Arc::clone(&runtime.inner), id.clone());
        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(resolution)) => {
                guard.disarm();
                Some(resolution)
            }
            // Operator channel closed (runtime teardown): fall back and leave
            // the registry clean.
            Ok(Err(_)) => {
                runtime
                    .settle_acp_interaction(&id, AcpSettledReason::Cancelled)
                    .await;
                guard.disarm();
                None
            }
            // Bound elapsed: fall back deterministically and drop the entry so
            // the pending list only ever reports interactions still awaiting an
            // operator answer.
            Err(_) => {
                runtime
                    .settle_acp_interaction(&id, AcpSettledReason::TimedOut)
                    .await;
                guard.disarm();
                None
            }
        }
    }
}

/// Removes a pending interaction when the asking future is dropped.
///
/// `ask` disarms the guard after every normal settlement (answer, timeout, or
/// channel close). It fires only when the turn future itself is cancelled, so
/// a dropped future cannot leak a stale pending interaction.
struct PendingInteractionGuard {
    runtime: Arc<super::RuntimeInner>,
    id: String,
    armed: bool,
}

impl PendingInteractionGuard {
    fn new(runtime: Arc<super::RuntimeInner>, id: String) -> Self {
        Self {
            runtime,
            id,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PendingInteractionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let runtime = Arc::clone(&self.runtime);
        let id = std::mem::take(&mut self.id);
        // Fast path: the registry lock is usually uncontended on cancellation.
        if let Ok(mut pending) = runtime.acp_interactions.try_lock() {
            if pending.remove(&id).is_some() {
                if let Ok(mut settled) = runtime.acp_settled_interactions.try_lock() {
                    insert_settled_interaction(&mut settled, id, AcpSettledReason::Cancelled);
                }
            }
            return;
        }
        // Slow path: hand the removal to a task that can await the lock.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if runtime.acp_interactions.lock().await.remove(&id).is_some() {
                    let mut settled = runtime.acp_settled_interactions.lock().await;
                    insert_settled_interaction(&mut settled, id, AcpSettledReason::Cancelled);
                }
            });
        }
    }
}

#[async_trait::async_trait]
impl AcpInteractionChannel for AcpInteractionBroker {
    async fn ask_permission(
        &self,
        session_id: &str,
        request: &PermissionRequest,
        scope: AcpGrantScope,
    ) -> Option<AcpPermissionDecisionKind> {
        self.broker_permission(session_id, request, scope).await
    }

    async fn ask_elicitation(
        &self,
        session_id: &str,
        request: &ElicitationRequest,
    ) -> Option<AcpInteractionResolution> {
        self.broker_elicitation(session_id, request).await
    }
}

impl super::RuntimeHandle {
    async fn register_acp_interaction(&self, pending: PendingAcpInteraction) -> Result<()> {
        let interaction_id = pending.id.clone();
        let view = pending.to_value();
        self.inner
            .acp_interactions
            .lock()
            .await
            .insert(interaction_id.clone(), pending);
        self.append_audit_event(
            "acp_interaction_requested",
            json!({
                "driver": "acp",
                "interaction_id": interaction_id,
                "interaction": view,
            }),
        )
        .await
    }

    /// Pending operator-facing ACP interactions for this agent.
    pub async fn pending_acp_interactions(&self) -> Vec<Value> {
        let interactions = self.inner.acp_interactions.lock().await;
        interactions
            .values()
            .map(PendingAcpInteraction::to_value)
            .collect()
    }

    /// Resolve a pending interaction from an operator-facing JSON body.
    ///
    /// Permission bodies use `{"decision": "allow_once" | "allow_always" |
    /// "reject_once" | "reject_always"}`. Elicitation bodies use
    /// `{"action": "accept", "content": {...}}`, `{"action": "decline"}`, or
    /// `{"action": "cancel"}`.
    pub async fn resolve_acp_interaction_decision(
        &self,
        interaction_id: &str,
        body: &Value,
    ) -> Result<AcpInteractionOutcome> {
        let resolution = parse_interaction_resolution(body)?;
        self.resolve_acp_interaction(interaction_id, resolution)
            .await
    }

    /// Remove a still-pending interaction and record why it settled.
    ///
    /// Used by the timeout, channel-close, and cancellation paths so the
    /// pending list never reports an interaction that is no longer waiting.
    async fn settle_acp_interaction(&self, interaction_id: &str, reason: AcpSettledReason) {
        let removed = self
            .inner
            .acp_interactions
            .lock()
            .await
            .remove(interaction_id)
            .is_some();
        if removed {
            self.record_acp_interaction_settled(interaction_id, reason)
                .await;
        }
    }

    async fn record_acp_interaction_settled(&self, interaction_id: &str, reason: AcpSettledReason) {
        let mut settled = self.inner.acp_settled_interactions.lock().await;
        insert_settled_interaction(&mut settled, interaction_id.to_string(), reason);
    }

    async fn acp_interaction_settled_outcome(&self, interaction_id: &str) -> AcpInteractionOutcome {
        let settled = self.inner.acp_settled_interactions.lock().await;
        match settled.get(interaction_id) {
            Some(entry) => AcpInteractionOutcome::AlreadySettled(entry.reason.as_str()),
            None => AcpInteractionOutcome::Unknown,
        }
    }

    /// Resolve one pending ACP interaction.
    ///
    /// A late resolution never creates authority; the gate has already applied
    /// its timeout fallback. The returned outcome lets the control plane explain
    /// an already-settled interaction instead of pretending it never existed.
    pub(crate) async fn resolve_acp_interaction(
        &self,
        interaction_id: &str,
        resolution: AcpInteractionResolution,
    ) -> Result<AcpInteractionOutcome> {
        {
            let interactions = self.inner.acp_interactions.lock().await;
            let Some(existing) = interactions.get(interaction_id) else {
                drop(interactions);
                return Ok(self.acp_interaction_settled_outcome(interaction_id).await);
            };
            let expected_kind = existing.request.kind();
            let actual_kind = match &resolution {
                AcpInteractionResolution::Permission(_) => "permission",
                AcpInteractionResolution::Elicitation(_) => "elicitation",
            };
            if expected_kind != actual_kind {
                return Err(anyhow!(
                    "interaction `{interaction_id}` is `{expected_kind}`, not `{actual_kind}`"
                ));
            }
        }
        let pending = self
            .inner
            .acp_interactions
            .lock()
            .await
            .remove(interaction_id);
        let Some(pending) = pending else {
            return Ok(self.acp_interaction_settled_outcome(interaction_id).await);
        };
        let accepted = pending.responder.send(resolution.clone()).is_ok();
        if accepted {
            self.record_acp_interaction_settled(interaction_id, AcpSettledReason::Resolved)
                .await;
        } else {
            // The waiter is gone; treat it like a timeout so a follow-up answer
            // is not mistaken for authority.
            self.record_acp_interaction_settled(interaction_id, AcpSettledReason::TimedOut)
                .await;
        }
        self.append_audit_event(
            "acp_interaction_resolved",
            json!({
                "driver": "acp",
                "interaction_id": interaction_id,
                "resolution": match &resolution {
                    AcpInteractionResolution::Permission(decision) => json!({
                        "kind": "permission",
                        "decision": match decision {
                            AcpPermissionDecisionKind::AllowOnce => "allow_once",
                            AcpPermissionDecisionKind::AllowAlways => "allow_always",
                            AcpPermissionDecisionKind::RejectOnce => "reject_once",
                            AcpPermissionDecisionKind::RejectAlways => "reject_always",
                        },
                    }),
                    AcpInteractionResolution::Elicitation(response) => json!({
                        "kind": "elicitation",
                        "response": response.to_value().unwrap_or(Value::Null),
                    }),
                },
                "accepted": accepted,
            }),
        )
        .await?;
        Ok(if accepted {
            AcpInteractionOutcome::Resolved
        } else {
            AcpInteractionOutcome::AlreadySettled("timed_out")
        })
    }
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

/// Turn-dimension binding resolved before an ACP turn runs.
///
/// ACP assistant rounds must carry the same turn keys as a native round so
/// turn-scoped transcript reads, turn revisions, and the agent-message reply
/// gate can find them. The settle path resolves this once and passes it in;
/// the evidence writer never re-reads mutable agent state on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpTurnMetadata {
    pub(crate) turn_id: String,
    pub(crate) turn_index: u64,
    pub(crate) work_item_id: Option<String>,
}

impl AcpTurnMetadata {
    /// Resolve the turn binding with the same precedence `build_turn_record`
    /// uses: execution binding, then the turn's work item, then the agent's.
    ///
    /// `current_turn_id` must already be assigned. Synthesizing one here would
    /// split the evidence identity from the terminal identity.
    pub(crate) fn resolve(
        current_turn_id: Option<&str>,
        turn_index: u64,
        execution_work_item_id: Option<Option<&str>>,
        current_turn_work_item_id: Option<&str>,
        current_work_item_id: Option<&str>,
    ) -> Result<Self> {
        let turn_id = current_turn_id
            .map(str::trim)
            .filter(|turn_id| !turn_id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow!("ACP turn has no current_turn_id; refusing to synthesize a turn identity")
            })?;
        let work_item_id = match execution_work_item_id {
            // An execution binding is authoritative even when it carries no
            // work item, exactly like `build_turn_record`.
            Some(work_item_id) => work_item_id.map(str::to_owned),
            None => current_turn_work_item_id
                .map(str::to_owned)
                .or_else(|| current_work_item_id.map(str::to_owned)),
        };
        Ok(Self {
            turn_id,
            turn_index,
            work_item_id,
        })
    }
}

/// Build the ACP assistant-round entry for one settled turn.
///
/// The turn keys mirror a native assistant round so every turn-scoped reader
/// sees one shape regardless of execution driver.
fn acp_assistant_round_entry(
    agent_id: &str,
    projection: &AcpTurnProjection,
    stop_reason: &str,
    permission_decisions: Vec<Value>,
    session_id: &str,
    turn: &AcpTurnMetadata,
) -> crate::types::TranscriptEntry {
    crate::types::TranscriptEntry {
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
                "turn_id": turn.turn_id,
                "turn_index": turn.turn_index,
                "work_item_id": turn.work_item_id,
                "round_purpose": "agent_response",
                "visibility": "operator_visible",
                "blocks": projection.blocks,
                "usage": projection.usage,
                "evidence": projection.evidence,
                "permission_decisions": permission_decisions,
            }),
        )
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
            SessionUpdate::SubagentUpdate {
                session_id,
                title,
                description,
                state,
            } => {
                // Patch evidence only. The harness owns these child sessions;
                // Holon never mirrors them into its own supervision tree.
                projection.evidence.push(json!({
                    "format": ACP_PROVIDER_DATA_FORMAT,
                    "kind": "subagents",
                    "session_update": "subagent_update",
                    "update": {
                        "sessionId": session_id,
                        "title": title,
                        "description": description,
                        "state": state,
                    },
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
    permission_mode: AcpPermissionMode,
    interaction_timeout: Duration,
    /// Whether the launch suppresses the harness' own upstream telemetry.
    disable_telemetry: bool,
    /// Whether the launch may pass the profile's declared permission-bypass
    /// flag. The static gate still rejects any undeclared bypass argument.
    dangerously_skip_permissions: bool,
    /// Where `dangerously_skip_permissions` came from, for audit evidence.
    permission_bypass_source: AcpPermissionBypassSource,
}

/// Where the permission-bypass decision came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcpPermissionBypassSource {
    /// `acp.profiles.<name>.dangerously_skip_permissions` is unset.
    ProfileDefault,
    /// The profile sets no bypass switch and `permission_mode` is
    /// `interactive`, so the bypass stays closed to keep operator approval.
    InteractiveDefault,
    /// The profile configuration sets the switch explicitly.
    Config,
}

impl AcpPermissionBypassSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProfileDefault => "profile_default",
            Self::InteractiveDefault => "interactive_default",
            Self::Config => "config",
        }
    }
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

    let permission_mode =
        AcpPermissionMode::parse(overrides.and_then(|profile| profile.permission_mode.as_deref()))?;
    let interaction_timeout = config.interaction_timeout();
    if interaction_timeout.is_zero() {
        return Err(anyhow!(
            "ACP interaction_timeout_ms must be greater than zero"
        ));
    }

    // Telemetry is suppressed by default for profiles that declare the flag.
    let disable_telemetry = config.disable_telemetry(&binding.profile);
    // The permission bypass is open by default for unattended turns, but the
    // bypass flag also auto-approves every harness-side permission request.
    // Interactive approval therefore stays closed unless the profile opens the
    // switch explicitly, and an explicit value always wins. Both switches stay
    // configurable per profile so an operator can restore upstream behaviour.
    let (dangerously_skip_permissions, permission_bypass_source) =
        match config.permission_bypass_override(&binding.profile) {
            Some(value) => (value, AcpPermissionBypassSource::Config),
            None if permission_mode.is_interactive() => {
                (false, AcpPermissionBypassSource::InteractiveDefault)
            }
            None => (true, AcpPermissionBypassSource::ProfileDefault),
        };

    Ok(ResolvedProfile {
        spec,
        role,
        env,
        sandbox,
        permission_mode,
        interaction_timeout,
        disable_telemetry,
        dangerously_skip_permissions,
        permission_bypass_source,
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
        disable_telemetry: profile.disable_telemetry,
        dangerously_skip_permissions: profile.dangerously_skip_permissions,
    };
    Ok((request, harness_env(&profile.env)))
}

/// Whether a launch argument asks the harness to stop gating its own tool
/// permission requests.
fn is_permission_bypass_arg(arg: &str) -> bool {
    arg.contains("dangerously-skip-permissions") || arg.contains("yolo")
}

/// Keep the harness permission gate static unless the profile explicitly
/// declares the bypass flag and the configuration opens it.
///
/// The gate is deliberately argument-level rather than a silent pass: a bypass
/// injected through `acp.profiles.<name>.args` still fails closed when the
/// per-profile switch is off, and the launched flag must be the profile-owned
/// one so the decision stays attributable.
fn ensure_permission_gate_preserved(
    launch: &holon_acp::harness::LaunchCommand,
    profile: &ResolvedProfile,
) -> Result<()> {
    let declared = profile.spec.dangerously_skip_permissions_arg.as_deref();
    for arg in &launch.args {
        if !is_permission_bypass_arg(arg) {
            continue;
        }
        ensure!(
            profile.dangerously_skip_permissions && declared == Some(arg.as_str()),
            "ACP harness `{}` may disable its permission gate only through the profile-declared flag `{}` \
             with `acp.profiles.{}.dangerously_skip_permissions = true`",
            profile.spec.name,
            declared.unwrap_or("<none>"),
            profile.spec.name,
        );
    }
    Ok(())
}

/// Convert one ACP client error into the runtime error surface.
///
/// An authentication failure is an operator-actionable state, not a generic
/// ACP failure: it names the harness that needs credentials, lists the auth
/// methods the peer advertised, and carries the harness' own recovery hint.
/// Provider credential wording never applies here.
fn acp_client_error(harness: &str, operation: &'static str, error: AcpError) -> anyhow::Error {
    if error.is_authentication_required() {
        return acp_authentication_error(harness, operation, &error).into();
    }
    anyhow!(error).context(operation)
}

/// Structured `auth_error` for a harness that rejected Holon as unauthenticated.
fn acp_authentication_error(
    harness: &str,
    operation: &'static str,
    error: &AcpError,
) -> crate::runtime_error::RuntimeError {
    use crate::runtime_error::{RuntimeError, RuntimeErrorDomain};

    let (message, auth_methods) = match error {
        AcpError::AuthenticationRequired {
            message,
            auth_methods,
        } => (message.as_str(), auth_methods.as_slice()),
        _ => ("authentication required", &[][..]),
    };
    let methods = if auth_methods.is_empty() {
        "no auth method advertised".to_string()
    } else {
        format!("auth methods: {}", auth_methods.join(", "))
    };
    let mut operator_message = format!(
        "ACP harness `{harness}` requires authentication: {} ({methods})",
        message.trim()
    );
    if let Some(hint) = error.recovery_hint() {
        operator_message.push_str("; ");
        operator_message.push_str(hint);
    }
    let runtime_error =
        RuntimeError::new(RuntimeErrorDomain::Runtime, "auth_error", operator_message)
            .with_safe_context("stage", "acp_authentication")
            // The stage alone cannot tell `initialize` from `session/new`; keep
            // the failing operation so the handshake step stays diagnosable.
            .with_safe_context("operation", operation);
    match error.recovery_hint() {
        Some(hint) => runtime_error.with_recovery_hint(hint),
        None => runtime_error,
    }
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
    ensure_permission_gate_preserved(&launch, profile)?;

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
    bound_interactive: bool,
    /// Launch switches the resident connection was opened with. Config can be
    /// hot-reloaded while the process keeps running, so reuse must re-check
    /// them instead of trusting a launch decision the process no longer has.
    bound_disable_telemetry: bool,
    bound_dangerously_skip_permissions: bool,
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

    /// Swap the ACP config the way a hot config reload would for the next turn.
    pub(crate) fn set_acp_config_for_test(&self, acp: crate::config::AcpConfigFile) {
        let mut snapshot = (**self.inner.config_snapshot.load()).clone();
        snapshot.acp = acp;
        self.inner.config_snapshot.store(Arc::new(snapshot));
    }

    async fn test_acp_harness(&self, gate: Arc<AcpPermissionGate>) -> Option<SpawnedHarness> {
        let factory = self.inner.acp_test_harness.lock().await.clone()?;
        Some(factory(gate as Arc<dyn AcpClientHandler>).into())
    }
}

impl super::RuntimeHandle {
    /// Record the explicit launch decisions that change harness-side behaviour.
    ///
    /// Suppressing harness telemetry and opening the permission gate are
    /// deliberate departures from the harness defaults. Each turn leaves one
    /// attributable marker (profile, turn, decision source) instead of relying
    /// on argv inspection after the fact.
    async fn record_acp_launch_decision(
        &self,
        agent_id: &str,
        binding: &AcpDriverBinding,
        profile: &ResolvedProfile,
        turn: &AcpTurnMetadata,
    ) -> Result<()> {
        if !profile.dangerously_skip_permissions
            || profile.spec.dangerously_skip_permissions_arg.is_none()
        {
            return Ok(());
        }
        self.append_audit_event(
            "acp_permission_gate_disabled",
            json!({
                "driver": "acp",
                "agent_id": agent_id,
                "acp_profile": binding.profile,
                "turn_id": turn.turn_id,
                "turn_index": turn.turn_index,
                "work_item_id": turn.work_item_id,
                "flag": profile.spec.dangerously_skip_permissions_arg,
                "decision_source": profile.permission_bypass_source.as_str(),
                "permission_mode": profile.permission_mode.as_str(),
                "telemetry_disabled": profile.disable_telemetry,
            }),
        )
        .await
        .context("record ACP launch decision")
    }

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
            // Resuming the same logical session keeps its grants. A different
            // session id starts with none.
            let session_grants = state
                .acp_session_state
                .as_ref()
                .filter(|existing| existing.resume_session_id == session_id)
                .map(|existing| existing.session_grants.clone())
                .unwrap_or_default();
            state.acp_session_state = Some(Box::new(crate::types::AcpSessionState {
                acp_profile: profile,
                resume_session_id: session_id,
                bound_execution_root,
                bound_additional_dirs,
                session_grants,
            }));
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Persist the session grants recorded during one turn.
    async fn persist_acp_session_grants(
        &self,
        session_id: &str,
        grants: Vec<crate::types::AcpSessionGrant>,
    ) -> Result<()> {
        let session_id = session_id.to_string();
        self.update_agent_state(|state| {
            if let Some(existing) = state.acp_session_state.as_mut() {
                if existing.resume_session_id == session_id {
                    existing.session_grants = grants;
                }
            }
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
                && resident.bound_interactive == gate.mode.is_interactive()
                && resident.bound_disable_telemetry == profile.disable_telemetry
                && resident.bound_dangerously_skip_permissions
                    == profile.dangerously_skip_permissions
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
        // Plan and subagents are read-only observation. Elicitation is only
        // advertised when this turn can actually wait for an operator answer.
        let mut capabilities = ClientCapabilities::trimmed()
            .with_plan_observation()
            .with_subagent_observation();
        if gate.mode.is_interactive() {
            capabilities = capabilities.with_elicitation_form();
        }
        let handshake = match client.initialize(InitializeParams::new(capabilities)).await {
            Ok(handshake) => handshake,
            Err(error) => {
                reap_process(&process, group.as_ref()).await;
                return Err(acp_client_error(
                    &profile.spec.name,
                    "ACP initialize failed",
                    error,
                ));
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
                            .map_err(|error| {
                                acp_client_error(
                                    &profile.spec.name,
                                    "ACP session/new failed",
                                    error,
                                )
                            })?
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
                                        acp_client_error(
                                            &profile.spec.name,
                                            "ACP session/new failed",
                                            error,
                                        )
                                    })?
                            }
                            Err(error) => {
                                reap_process(&process, group.as_ref()).await;
                                return Err(acp_client_error(
                                    &profile.spec.name,
                                    "ACP session/load failed",
                                    error,
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        reap_process(&process, group.as_ref()).await;
                        return Err(acp_client_error(
                            &profile.spec.name,
                            "ACP session/resume failed",
                            error,
                        ));
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
                            .map_err(|error| {
                                acp_client_error(
                                    &profile.spec.name,
                                    "ACP session/new failed",
                                    error,
                                )
                            })?
                    }
                    Err(error) => {
                        reap_process(&process, group.as_ref()).await;
                        return Err(acp_client_error(
                            &profile.spec.name,
                            "ACP session/load failed",
                            error,
                        ));
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
                    .map_err(|error| {
                        acp_client_error(&profile.spec.name, "ACP session/new failed", error)
                    })?
            }
        } else {
            client
                .new_session(session_new_params(execution, profile, &handshake))
                .await
                .map_err(|error| {
                    acp_client_error(&profile.spec.name, "ACP session/new failed", error)
                })?
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

        // Grants belong to the logical session credential. A resumed session
        // keeps them; a new or rebuilt session starts empty so `allow_always`
        // can never transfer across a rotation.
        let session_grants = if !new_session {
            state
                .acp_session_state
                .as_ref()
                .filter(|persisted| persisted.resume_session_id == session.session_id)
                .map(|persisted| persisted.session_grants.clone())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        gate.load_grants(&session_grants);

        let bound_writable = gate.writable;
        let bound_process_execution_exposed = gate.process_execution_exposed;
        let bound_interactive = gate.mode.is_interactive();
        let bound_disable_telemetry = profile.disable_telemetry;
        let bound_dangerously_skip_permissions = profile.dangerously_skip_permissions;
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
                bound_interactive,
                bound_disable_telemetry,
                bound_dangerously_skip_permissions,
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
        turn: &AcpTurnMetadata,
        cancel: CancellationToken,
    ) -> Result<AcpTurnResult> {
        let config = self.inner.config_snapshot.load().acp.clone();
        let acp_config = &config;
        let profile = resolve_profile(acp_config, binding, execution)?;
        self.record_acp_launch_decision(agent_id, binding, &profile, turn)
            .await?;
        let interaction: Option<Arc<dyn AcpInteractionChannel>> =
            profile.permission_mode.is_interactive().then(|| {
                Arc::new(AcpInteractionBroker::new(
                    Arc::downgrade(&self.inner),
                    agent_id,
                    profile.interaction_timeout,
                )) as Arc<dyn AcpInteractionChannel>
            });
        let gate = Arc::new(AcpPermissionGate::new(
            execution,
            profile.permission_mode,
            interaction,
        ));
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
        if let Err(error) = self
            .persist_acp_session_grants(session_id.as_str(), session.gate.grants())
            .await
        {
            if matches!(&outcome, Outcome::Settled(Ok(_))) {
                control.cancel_and_reap().await;
            }
            return Err(error);
        }
        if let Err(error) = self
            .record_acp_projection_events(agent_id, session_id.as_str(), &projection)
            .await
        {
            if matches!(&outcome, Outcome::Settled(Ok(_))) {
                control.cancel_and_reap().await;
            }
            return Err(error);
        }
        let transcript_entry_id = match self
            .persist_acp_evidence(
                agent_id,
                &projection,
                &stop_reason,
                session.gate.drain(),
                session_id.as_str(),
                turn,
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
                return Err(acp_client_error(
                    &profile.spec.name,
                    "ACP session/prompt failed",
                    error,
                ));
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

    /// Append one event per observable ACP update family.
    ///
    /// Plan, subagent, and usage updates are evidence only. They never create
    /// model blocks, tool dispatch, or Holon supervision. The audited payload
    /// keeps the `acp_v1` marker so consumers can apply the wire contract.
    async fn record_acp_projection_events(
        &self,
        agent_id: &str,
        session_id: &str,
        projection: &AcpTurnProjection,
    ) -> Result<()> {
        for evidence in &projection.evidence {
            match evidence.get("kind").and_then(Value::as_str) {
                Some("plan") => {
                    self.append_audit_event(
                        "acp_plan_updated",
                        json!({
                            "driver": "acp",
                            "format": ACP_PROVIDER_DATA_FORMAT,
                            "agent_id": agent_id,
                            "session_id": session_id,
                            "plan": evidence.get("entries").cloned().unwrap_or(Value::Null),
                        }),
                    )
                    .await?;
                }
                Some("subagents") => {
                    self.append_audit_event(
                        "acp_subagents_updated",
                        json!({
                            "driver": "acp",
                            "format": ACP_PROVIDER_DATA_FORMAT,
                            "agent_id": agent_id,
                            "session_id": session_id,
                            "subagent": evidence.get("update").cloned().unwrap_or(Value::Null),
                        }),
                    )
                    .await?;
                }
                _ => {}
            }
        }
        if let Some(usage) = &projection.usage {
            self.append_audit_event(
                "acp_usage_updated",
                json!({
                    "driver": "acp",
                    "format": ACP_PROVIDER_DATA_FORMAT,
                    "agent_id": agent_id,
                    "session_id": session_id,
                    "usage": usage,
                }),
            )
            .await?;
        }
        Ok(())
    }

    /// Append the ACP turn evidence to the agent ledger.
    async fn persist_acp_evidence(
        &self,
        agent_id: &str,
        projection: &AcpTurnProjection,
        stop_reason: &str,
        permission_decisions: Vec<Value>,
        session_id: &str,
        turn: &AcpTurnMetadata,
    ) -> Result<String> {
        let entry = acp_assistant_round_entry(
            agent_id,
            projection,
            stop_reason,
            permission_decisions,
            session_id,
            turn,
        );
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

    fn unattended_gate(execution: &EffectiveExecution) -> AcpPermissionGate {
        AcpPermissionGate::new(execution, AcpPermissionMode::Unattended, None)
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

    #[tokio::test]
    async fn permission_gate_rejects_paths_outside_the_bound_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = unattended_gate(&execution(dir.path(), true));
        let outside = gate
            .decide(&PermissionRequest {
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
            })
            .await;
        assert!(
            matches!(outside, PermissionOutcome::Selected { option_id } if option_id == "reject_once")
        );
        assert_eq!(
            gate.recorded()[0]["reason"],
            "path_outside_bound_execution_root"
        );

        let traversal = format!("{}/../etc/passwd", dir.path().display());
        let rejected_traversal = gate
            .decide(&PermissionRequest {
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
            })
            .await;
        assert!(
            matches!(rejected_traversal, PermissionOutcome::Selected { option_id } if option_id == "reject_once")
        );
        assert_eq!(
            gate.recorded()[1]["reason"],
            "path_outside_bound_execution_root"
        );

        let inside = dir.path().join("src/lib.rs");
        let allowed = gate
            .decide(&PermissionRequest {
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
            })
            .await;
        assert!(
            matches!(allowed, PermissionOutcome::Selected { option_id } if option_id == "allow_once")
        );
    }

    #[tokio::test]
    async fn read_only_scope_rejects_write_kinds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = unattended_gate(&execution(dir.path(), false));
        let outcome = gate
            .decide(&PermissionRequest {
                session_id: "s".into(),
                tool_call: json!({"toolCallId": "call-3", "kind": "edit"}),
                options: vec![PermissionOption {
                    option_id: "reject_once".into(),
                    name: "Reject".into(),
                    kind: None,
                }],
            })
            .await;
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
            session_grants: Vec::new(),
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
    #[derive(Clone, Default)]
    struct TestPeerConfig {
        supports_resume: bool,
        supports_load: bool,
        resume_missing: bool,
        /// Error response for `session/prompt`, used to exercise ACP failure paths.
        prompt_error: Option<holon_acp::RpcError>,
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
                    if let Some(error) = config.prompt_error.clone() {
                        write_peer_frame(&mut write_half, Frame::Error { id, error }).await;
                        continue;
                    }
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
        let turn = AcpTurnMetadata::resolve(Some("turn-acp-test"), 0, None, None, None)
            .expect("turn metadata");
        runtime
            .run_acp_turn(
                "default",
                prompt,
                execution,
                &antigravity_binding(),
                &turn,
                CancellationToken::new(),
            )
            .await
            .expect("ACP turn")
            .text
    }

    fn acp_config(
        disable_telemetry: Option<bool>,
        dangerously_skip_permissions: Option<bool>,
    ) -> crate::config::AcpConfigFile {
        let mut overrides = crate::config::AcpHarnessProfileConfigFile::default();
        overrides.disable_telemetry = disable_telemetry;
        overrides.dangerously_skip_permissions = dangerously_skip_permissions;
        let mut config = crate::config::AcpConfigFile::default();
        config
            .profiles
            .insert(holon_acp::antigravity::NAME.to_string(), overrides);
        config
    }

    fn acp_config_with_mode(
        permission_mode: &str,
        dangerously_skip_permissions: Option<bool>,
    ) -> crate::config::AcpConfigFile {
        let mut overrides = crate::config::AcpHarnessProfileConfigFile::default();
        overrides.permission_mode = Some(permission_mode.to_string());
        overrides.dangerously_skip_permissions = dangerously_skip_permissions;
        let mut config = crate::config::AcpConfigFile::default();
        config
            .profiles
            .insert(holon_acp::antigravity::NAME.to_string(), overrides);
        config
    }

    #[test]
    fn launch_defaults_disable_telemetry_and_open_the_permission_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exec = execution(dir.path(), true);
        let profile = resolve_profile(
            &crate::config::AcpConfigFile::default(),
            &antigravity_binding(),
            &exec,
        )
        .expect("profile");
        assert!(profile.disable_telemetry);
        assert!(profile.dangerously_skip_permissions);
        assert_eq!(
            profile.permission_bypass_source,
            AcpPermissionBypassSource::ProfileDefault
        );

        let (request, _env) = launch_request(&profile, &exec).expect("launch request");
        let launch = profile.spec.build_launch(&request);
        assert!(launch.args.iter().any(|arg| arg == "--disable-telemetry"));
        assert!(launch
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        ensure_permission_gate_preserved(&launch, &profile).expect("declared bypass is admitted");
    }

    #[test]
    fn explicit_profile_config_restores_telemetry_and_the_static_gate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exec = execution(dir.path(), true);
        let profile = resolve_profile(
            &acp_config(Some(false), Some(false)),
            &antigravity_binding(),
            &exec,
        )
        .expect("profile");
        assert!(!profile.disable_telemetry);
        assert!(!profile.dangerously_skip_permissions);
        assert_eq!(
            profile.permission_bypass_source,
            AcpPermissionBypassSource::Config
        );

        let (request, _env) = launch_request(&profile, &exec).expect("launch request");
        let launch = profile.spec.build_launch(&request);
        assert!(!launch.args.iter().any(|arg| arg == "--disable-telemetry"));
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        ensure_permission_gate_preserved(&launch, &profile).expect("nothing to reject");

        // A bypass injected through `acp.profiles.<name>.args` still fails
        // closed once the per-profile switch is off.
        let mut injected = launch.clone();
        injected.args.push("--dangerously-skip-permissions".into());
        let error = ensure_permission_gate_preserved(&injected, &profile)
            .expect_err("an undeclared bypass must be rejected");
        assert!(error.to_string().contains("dangerously_skip_permissions"));
    }

    #[tokio::test]
    async fn permission_gate_decision_is_audited_once_per_turn() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: false,
                ..TestPeerConfig::default()
            },
        )
        .await;
        let root = tempdir().unwrap();
        let exec = execution(root.path(), true);

        turn(&runtime, &exec, "audit the launch decision").await;

        let connection = runtime.runtime_db().connection().unwrap();
        let (count, payload): (i64, String) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(data_json), '') FROM audit_events \
                 WHERE kind = 'acp_permission_gate_disabled'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "one explicit marker per turn");
        let payload: Value = serde_json::from_str(&payload).expect("audit payload");
        let data = &payload["data"];
        assert_eq!(data["acp_profile"], "antigravity");
        assert_eq!(data["turn_id"], "turn-acp-test");
        assert_eq!(data["flag"], "--dangerously-skip-permissions");
        assert_eq!(data["decision_source"], "profile_default");
        assert_eq!(data["permission_mode"], "unattended");
        assert_eq!(data["telemetry_disabled"], true);
    }

    #[test]
    fn interactive_permission_mode_keeps_the_bypass_closed_by_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exec = execution(dir.path(), true);
        let profile = resolve_profile(
            &acp_config_with_mode("interactive", None),
            &antigravity_binding(),
            &exec,
        )
        .expect("profile");

        // Operator approval is the only gate interactive mode has, so the
        // harness' own auto-approval stays off until a profile opens it.
        assert!(profile.permission_mode.is_interactive());
        assert!(!profile.dangerously_skip_permissions);
        assert_eq!(
            profile.permission_bypass_source,
            AcpPermissionBypassSource::InteractiveDefault
        );
        assert!(profile.disable_telemetry);
        let (request, _env) = launch_request(&profile, &exec).expect("launch request");
        let launch = profile.spec.build_launch(&request);
        assert!(!launch
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        ensure_permission_gate_preserved(&launch, &profile).expect("nothing to admit");

        // An explicit value still wins and is attributed to configuration.
        let profile = resolve_profile(
            &acp_config_with_mode("interactive", Some(true)),
            &antigravity_binding(),
            &exec,
        )
        .expect("profile");
        assert!(profile.dangerously_skip_permissions);
        assert_eq!(
            profile.permission_bypass_source,
            AcpPermissionBypassSource::Config
        );
        let (request, _env) = launch_request(&profile, &exec).expect("launch request");
        let launch = profile.spec.build_launch(&request);
        assert!(launch
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
    }

    #[tokio::test]
    async fn harness_authentication_failure_settles_as_structured_auth_error() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        install_test_peer(
            &runtime,
            TestPeerConfig {
                prompt_error: Some(holon_acp::RpcError {
                    code: holon_acp::AUTHENTICATION_REQUIRED,
                    message: "Authentication required".into(),
                    data: Some(json!({
                        "authMethods": [{"id": "oauth-personal"}, {"id": "terminal-login"}],
                    })),
                }),
                ..TestPeerConfig::default()
            },
        )
        .await;
        let root = tempdir().unwrap();
        let exec = execution(root.path(), true);
        let turn_metadata = AcpTurnMetadata::resolve(Some("turn-acp-test"), 0, None, None, None)
            .expect("turn metadata");

        let outcome = runtime
            .run_acp_turn(
                "default",
                "authenticate me",
                &exec,
                &antigravity_binding(),
                &turn_metadata,
                CancellationToken::new(),
            )
            .await;
        let error = match outcome {
            Ok(_) => panic!("the harness rejected the prompt as unauthenticated"),
            Err(error) => error,
        };

        let descriptor = crate::runtime_error::describe_runtime_error(&error);
        assert_eq!(descriptor.code, "auth_error");
        assert!(!descriptor.retryable);
        assert!(
            descriptor.operator_message.contains("antigravity"),
            "the message names the harness: {}",
            descriptor.operator_message
        );
        assert!(
            descriptor.operator_message.contains("oauth-personal")
                && descriptor.operator_message.contains("terminal-login"),
            "the message carries the advertised auth methods: {}",
            descriptor.operator_message
        );
        assert!(
            descriptor.operator_message.contains("agy acp --login"),
            "the message carries the harness recovery hint: {}",
            descriptor.operator_message
        );
        assert!(descriptor
            .recovery_hint
            .as_deref()
            .is_some_and(|hint| hint.contains("agy acp --login")));
        assert!(
            !descriptor
                .operator_message
                .contains("check provider authentication"),
            "provider credential wording must not leak into harness auth failures"
        );
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
                prompt_error: None,
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
                prompt_error: None,
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
                prompt_error: None,
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
                prompt_error: None,
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
                prompt_error: None,
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
    async fn reloaded_launch_switches_rebuild_the_resident_session() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let peer = install_test_peer(
            &runtime,
            TestPeerConfig {
                supports_resume: true,
                supports_load: false,
                resume_missing: false,
                prompt_error: None,
            },
        )
        .await;
        let root = tempdir().unwrap();
        let exec = execution(root.path(), true);

        turn(&runtime, &exec, "first request").await;
        let persisted = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state is persisted");

        // A hot config reload closes the bypass, but the running process still
        // carries the old flag; the resident must be rebuilt for the decision
        // to take effect and to keep the audit evidence truthful.
        runtime.set_acp_config_for_test(acp_config(None, Some(false)));
        turn(&runtime, &exec, "second request").await;

        assert_eq!(
            peer.handshakes(),
            2,
            "a switched launch decision must not reuse the old process"
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
                prompt_error: None,
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

    #[tokio::test]
    async fn permission_gate_drains_recorded_decisions_between_turns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = unattended_gate(&execution(dir.path(), true));
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
            gate.decide(&request).await,
            PermissionOutcome::Selected { .. }
        ));
        assert!(matches!(
            gate.decide(&request).await,
            PermissionOutcome::Selected { .. }
        ));
        assert_eq!(gate.drain().len(), 2, "one turn sees only its decisions");
        assert!(
            gate.drain().is_empty(),
            "a resident gate must not replay earlier turns"
        );
    }

    fn read_request(id: &str, path: &std::path::Path) -> PermissionRequest {
        PermissionRequest {
            session_id: "s".into(),
            tool_call: json!({
                "toolCallId": id,
                "kind": "read",
                "rawInput": {"path": path},
            }),
            options: vec![
                PermissionOption {
                    option_id: "allow_once".into(),
                    name: "Allow once".into(),
                    kind: None,
                },
                PermissionOption {
                    option_id: "allow_always".into(),
                    name: "Allow for session".into(),
                    kind: None,
                },
            ],
        }
    }

    #[tokio::test]
    async fn session_grant_matches_only_the_same_tool_kind_and_path_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = unattended_gate(&execution(dir.path(), true));
        let inside = dir.path().join("src/lib.rs");
        let first = read_request("call-1", &inside);
        let _ = gate.decide(&first).await;
        gate.record_grant(gate.request_scope(&first));

        let repeated = read_request("call-2", &inside);
        assert!(matches!(
            gate.decide(&repeated).await,
            PermissionOutcome::Selected { .. }
        ));
        let decisions = gate.drain();
        assert_eq!(decisions.last().unwrap()["reason"], "session_grant_matches");
        assert_eq!(gate.grants().len(), 1);

        // A different path is a different scope, so it falls back to the
        // ordinary inside-root decision instead of reusing the grant.
        let other = read_request("call-3", &dir.path().join("src/other.rs"));
        assert!(matches!(
            gate.decide(&other).await,
            PermissionOutcome::Selected { .. }
        ));
        assert_eq!(
            gate.drain()[0]["reason"],
            "request_stays_inside_the_bound_execution_scope"
        );
    }

    struct FakeInteraction {
        permission: Option<AcpPermissionDecisionKind>,
        elicitation: Option<AcpInteractionResolution>,
        permission_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AcpInteractionChannel for FakeInteraction {
        async fn ask_permission(
            &self,
            _session_id: &str,
            _request: &PermissionRequest,
            _scope: AcpGrantScope,
        ) -> Option<AcpPermissionDecisionKind> {
            self.permission_calls.fetch_add(1, Ordering::SeqCst);
            self.permission
        }

        async fn ask_elicitation(
            &self,
            _session_id: &str,
            _request: &ElicitationRequest,
        ) -> Option<AcpInteractionResolution> {
            self.elicitation.clone()
        }
    }

    fn interactive_gate(
        execution: &EffectiveExecution,
        interaction: Arc<FakeInteraction>,
    ) -> AcpPermissionGate {
        AcpPermissionGate::new(execution, AcpPermissionMode::Interactive, Some(interaction))
    }

    #[tokio::test]
    async fn interactive_allow_always_records_one_session_grant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let interaction = Arc::new(FakeInteraction {
            permission: Some(AcpPermissionDecisionKind::AllowAlways),
            elicitation: None,
            permission_calls: AtomicUsize::new(0),
        });
        let gate = interactive_gate(&execution(dir.path(), true), Arc::clone(&interaction));
        let request = read_request("call-1", &dir.path().join("src/lib.rs"));
        assert!(matches!(
            gate.decide(&request).await,
            PermissionOutcome::Selected { option_id } if option_id == "allow_always"
        ));
        assert_eq!(gate.grants().len(), 1);

        let repeated = read_request("call-2", &dir.path().join("src/lib.rs"));
        assert!(matches!(
            gate.decide(&repeated).await,
            PermissionOutcome::Selected { .. }
        ));
        assert_eq!(
            interaction.permission_calls.load(Ordering::SeqCst),
            1,
            "a session grant must settle without asking again"
        );
    }

    #[tokio::test]
    async fn interactive_timeout_falls_back_to_rejection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let interaction = Arc::new(FakeInteraction {
            permission: None,
            elicitation: None,
            permission_calls: AtomicUsize::new(0),
        });
        let gate = interactive_gate(&execution(dir.path(), true), interaction);
        let outcome = gate
            .decide(&read_request("call-1", &dir.path().join("src/lib.rs")))
            .await;
        assert_eq!(outcome, PermissionOutcome::Cancelled);
        assert_eq!(
            gate.drain()[0]["reason"],
            "interaction_timeout_or_cancelled"
        );
    }

    #[test]
    fn parses_operator_interaction_resolution_bodies() {
        assert_eq!(
            parse_interaction_resolution(&json!({"decision": "allow_always"})).unwrap(),
            AcpInteractionResolution::Permission(AcpPermissionDecisionKind::AllowAlways)
        );
        assert_eq!(
            parse_interaction_resolution(&json!({"action": "accept", "content": {"a": 1}}))
                .unwrap(),
            AcpInteractionResolution::Elicitation(ElicitationResponse::Accept {
                content: Some(json!({"a": 1})),
            })
        );
        assert!(parse_interaction_resolution(&json!({"decision": "maybe"})).is_err());
        assert!(parse_interaction_resolution(&json!({})).is_err());
    }

    #[tokio::test]
    async fn timed_out_interaction_is_removed_from_the_pending_list() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let broker = AcpInteractionBroker::new(
            Arc::downgrade(&runtime.inner),
            "default",
            std::time::Duration::from_millis(20),
        );
        let request = ElicitationRequest {
            session_id: "s1".into(),
            mode: "form".into(),
            message: "need input".into(),
            requested_schema: Some(json!({"type": "object"})),
            tool_call_id: None,
        };
        assert_eq!(broker.ask_elicitation("s1", &request).await, None);
        assert!(
            runtime.pending_acp_interactions().await.is_empty(),
            "a timed-out interaction must not linger in the pending list"
        );
    }

    #[tokio::test]
    async fn resolution_distinguishes_settled_from_unknown() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let (responder, _receiver) = tokio::sync::oneshot::channel();
        let id = "acpint-settled";
        runtime
            .register_acp_interaction(PendingAcpInteraction {
                id: id.to_string(),
                agent_id: "default".into(),
                session_id: "s1".into(),
                created_at: chrono::Utc::now(),
                request: AcpInteractionRequest::Elicitation {
                    message: "q".into(),
                    requested_schema: None,
                    tool_call_id: None,
                },
                responder,
            })
            .await
            .expect("register interaction");
        runtime
            .settle_acp_interaction(id, AcpSettledReason::TimedOut)
            .await;
        assert!(runtime.pending_acp_interactions().await.is_empty());
        assert_eq!(
            runtime
                .resolve_acp_interaction_decision(id, &json!({"action": "decline"}))
                .await
                .expect("resolve settled"),
            AcpInteractionOutcome::AlreadySettled("timed_out")
        );
        assert_eq!(
            runtime
                .resolve_acp_interaction_decision("acpint-missing", &json!({"action": "decline"}))
                .await
                .expect("resolve unknown"),
            AcpInteractionOutcome::Unknown
        );
    }

    #[tokio::test]
    async fn elicitation_request_round_trips_through_the_control_plane() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let broker = AcpInteractionBroker::new(
            Arc::downgrade(&runtime.inner),
            "default",
            std::time::Duration::from_secs(5),
        );
        let request = ElicitationRequest {
            session_id: "s1".into(),
            mode: "form".into(),
            message: "choose".into(),
            requested_schema: Some(json!({"type": "object"})),
            tool_call_id: Some("call-1".into()),
        };
        let ask = broker.ask_elicitation("s1", &request);
        let answer = async {
            loop {
                let pending = runtime.pending_acp_interactions().await;
                if let Some(view) = pending.first() {
                    let id = view["interaction_id"]
                        .as_str()
                        .expect("interaction id")
                        .to_string();
                    return runtime
                        .resolve_acp_interaction_decision(
                            &id,
                            &json!({"action": "accept", "content": {"choice": "a"}}),
                        )
                        .await
                        .expect("resolve elicitation");
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        };
        let (asked, outcome) = tokio::join!(ask, answer);
        assert_eq!(outcome, AcpInteractionOutcome::Resolved);
        assert_eq!(
            asked,
            Some(AcpInteractionResolution::Elicitation(
                ElicitationResponse::Accept {
                    content: Some(json!({"choice": "a"})),
                }
            ))
        );
        assert!(
            runtime.pending_acp_interactions().await.is_empty(),
            "a resolved interaction leaves the pending list"
        );
    }

    #[tokio::test]
    async fn session_grants_survive_resume_and_drop_on_rotation() {
        let (_home, _workspace, runtime) = acp_continuity_runtime();
        let grant = crate::types::AcpSessionGrant {
            tool_kind: Some("read".into()),
            path_scope: Some("/root/src/lib.rs".into()),
            trust_domain: "/root".into(),
        };
        runtime
            .persist_acp_session_state("antigravity", "sess-1", "/root", &[])
            .await
            .expect("seed session state");
        runtime
            .persist_acp_session_grants("sess-1", vec![grant.clone()])
            .await
            .expect("persist grant");
        let stored = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state")
            .session_grants;
        assert_eq!(stored, vec![grant.clone()]);

        // Resuming the same logical session keeps its grants.
        runtime
            .persist_acp_session_state("antigravity", "sess-1", "/root", &[])
            .await
            .unwrap();
        let resumed = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state")
            .session_grants;
        assert_eq!(resumed, vec![grant.clone()], "resume keeps grants");

        // A different session id starts empty and rejects stale writes.
        runtime
            .persist_acp_session_state("antigravity", "sess-2", "/root", &[])
            .await
            .unwrap();
        runtime
            .persist_acp_session_grants("sess-1", vec![grant])
            .await
            .unwrap();
        let rotated = runtime
            .agent_state()
            .await
            .unwrap()
            .acp_session_state
            .expect("session state")
            .session_grants;
        assert!(
            rotated.is_empty(),
            "a rotated session must not inherit grants"
        );
    }

    #[tokio::test]
    async fn loaded_session_grants_answer_without_the_channel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let execution = execution(dir.path(), true);
        let interaction = Arc::new(FakeInteraction {
            permission: Some(AcpPermissionDecisionKind::AllowOnce),
            elicitation: None,
            permission_calls: AtomicUsize::new(0),
        });
        let gate = interactive_gate(&execution, Arc::clone(&interaction));
        let request = read_request("call-1", &dir.path().join("src/lib.rs"));
        let grant = gate.request_scope(&request).to_grant(&gate.trust_domain);
        gate.load_grants(&[grant]);
        assert!(matches!(
            gate.decide(&request).await,
            PermissionOutcome::Selected { .. }
        ));
        assert_eq!(gate.drain()[0]["reason"], "session_grant_matches");
        assert_eq!(interaction.permission_calls.load(Ordering::SeqCst), 0);

        // A fresh session loads an empty set, so the grant no longer applies.
        gate.load_grants(&[]);
        assert!(matches!(
            gate.decide(&request).await,
            PermissionOutcome::Selected { option_id } if option_id == "allow_once"
        ));
        assert_eq!(interaction.permission_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn plan_subagent_and_usage_updates_project_as_read_only_evidence() {
        let updates = vec![
            SessionUpdate::Plan {
                entries: vec![PlanEntry {
                    content: "step one".into(),
                    priority: None,
                    status: Some("pending".into()),
                }],
            },
            SessionUpdate::SubagentUpdate {
                session_id: "sub-1".into(),
                title: Some("child".into()),
                description: None,
                state: Some(json!("running")),
            },
            SessionUpdate::UsageUpdate {
                used: 10,
                size: 100,
                cost: None,
            },
        ];
        let projection = project_updates(&updates);
        assert!(
            projection.blocks.is_empty(),
            "plan/subagent/usage updates never create model blocks"
        );
        let plan = projection
            .evidence
            .iter()
            .find(|entry| entry["kind"] == "plan")
            .expect("plan evidence");
        assert_eq!(plan["entries"][0]["content"], "step one");
        let subagent = projection
            .evidence
            .iter()
            .find(|entry| entry["kind"] == "subagents")
            .expect("subagent evidence");
        assert_eq!(subagent["session_update"], "subagent_update");
        assert_eq!(subagent["update"]["sessionId"], "sub-1");
        let usage = projection.usage.as_ref().expect("usage projection");
        assert_eq!(usage["used"], 10);
        assert_eq!(usage["size"], 100);
    }

    fn acp_turn_metadata() -> AcpTurnMetadata {
        AcpTurnMetadata::resolve(Some("turn-acp"), 3, Some(Some("work-item-acp")), None, None)
            .expect("turn metadata")
    }

    #[test]
    fn acp_assistant_round_carries_native_turn_dimension_keys() {
        let projection = project_updates(&[SessionUpdate::AgentMessageChunk {
            content: ContentBlock::text("ACP-agy-e2e ok"),
        }]);
        let entry = acp_assistant_round_entry(
            "agent-a",
            &projection,
            "end_turn",
            Vec::new(),
            "session-1",
            &acp_turn_metadata(),
        );
        assert_eq!(
            entry.kind,
            crate::types::TranscriptEntryKind::AssistantRound
        );
        assert_eq!(entry.data["driver"], "acp");
        assert_eq!(entry.data["turn_id"], "turn-acp");
        assert_eq!(entry.data["turn_index"], 3);
        assert_eq!(entry.data["work_item_id"], "work-item-acp");
        assert_eq!(entry.data["round_purpose"], "agent_response");
        assert_eq!(entry.data["visibility"], "operator_visible");
        assert!(entry.data["blocks"]
            .as_array()
            .expect("blocks")
            .iter()
            .any(|block| block["type"] == "text" && block["text"] == "ACP-agy-e2e ok"));
    }

    #[test]
    fn acp_turn_metadata_requires_an_assigned_turn_id() {
        for missing in [None, Some(""), Some("   ")] {
            let error = AcpTurnMetadata::resolve(missing, 0, None, None, None)
                .expect_err("an unassigned ACP turn must fail fast");
            assert!(
                error.to_string().contains("current_turn_id"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn acp_turn_metadata_work_item_precedence_matches_turn_record() {
        let resolve = |execution, turn, agent| {
            AcpTurnMetadata::resolve(Some("turn-acp"), 1, execution, turn, agent)
                .expect("turn metadata")
                .work_item_id
        };
        assert_eq!(
            resolve(Some(Some("execution")), Some("turn"), Some("agent")).as_deref(),
            Some("execution")
        );
        assert_eq!(resolve(Some(None), Some("turn"), Some("agent")), None);
        assert_eq!(
            resolve(None, Some("turn"), Some("agent")).as_deref(),
            Some("turn")
        );
        assert_eq!(resolve(None, None, Some("agent")).as_deref(), Some("agent"));
        assert_eq!(resolve(None, None, None), None);
    }
}
