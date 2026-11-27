//! Pure domain types for the conversation read model.

use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::{ActivityKey, TurnKey};

use crate::{
    ContinuationTriggerKind, MessageKind, TurnNoBriefReason, TurnTerminalKind, TurnTriggerSummary,
};

pub const CONVERSATION_SCHEMA_VERSION: u32 = 2;
pub const CONVERSATION_QUERY_VERSION: u32 = 2;

/// Runtime task evidence, independently projected from any assistant response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskResultPresentation {
    pub task_id: String,
    pub status: crate::TaskStatus,
    pub summary: Option<String>,
    pub preview: String,
    pub response_message_id: Option<String>,
    /// True only when durable turn provenance identifies a runtime-only result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_only: Option<bool>,
}

impl TaskResultPresentation {
    pub fn from_message_payload(payload: Option<String>) -> Option<Self> {
        let message: crate::MessageEnvelope = serde_json::from_str(payload.as_deref()?).ok()?;
        if message.kind != MessageKind::TaskResult {
            return None;
        }
        let metadata = message.metadata.as_ref()?;
        let status = serde_json::from_value(metadata.get("task_status")?.clone()).ok()?;
        let preview = match &message.body {
            crate::MessageBody::Text { text } | crate::MessageBody::Brief { text, .. } => {
                text.clone()
            }
            crate::MessageBody::Json { value } => value.to_string(),
        }
        .chars()
        .take(320)
        .collect();
        Some(Self {
            runtime_only: None,
            task_id: metadata.get("task_id")?.as_str()?.to_owned(),
            status,
            summary: metadata
                .get("task_summary")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.chars().take(160).collect()),
            preview,
            response_message_id: crate::agent_message_reply_reference(&message).map(str::to_owned),
        })
    }
}

/// Canonical operator input attached to a turn, for summary-level rendering
/// without loading turn activity detail. Bounded per turn by the read model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnInputSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_result: Option<TaskResultPresentation>,
    pub message_id: String,
    pub preview: String,
    /// Operator display name snapshotted in the canonical message origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_class: Option<PresentationClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_key: Option<ActivityKey>,
    #[serde(default)]
    pub interjected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationTurnSummary {
    pub turn_id: String,
    pub key: TurnKey,
    pub revision: u64,
    pub presentation_class: PresentationClass,
    /// True when the turn's conversation content was deleted. The summary then
    /// carries identity, timing, and execution outcome only; inputs and brief
    /// ids are empty and must not be rehydrated from any source.
    #[serde(default)]
    pub content_deleted: bool,
    pub inputs: Vec<TurnInputSummary>,
    #[serde(default)]
    pub inputs_truncated: bool,
    pub execution: ExecutionState,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<u64>,
    pub result: ResultState,
    pub settled: bool,
    pub attention: Option<Attention>,
    pub detail_coverage: DetailCoverage,
    pub brief_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummaryPage {
    pub turns: Vec<ConversationTurnSummary>,
    pub active_turns: Vec<ConversationTurnSummary>,
    pub pending_inputs: Vec<PendingInput>,
    pub membership_upper_bound: Option<TurnKey>,
    pub next_before: Option<TurnKey>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationShadowDiagnostics {
    pub schema_version: u32,
    pub query_version: u32,
    pub runtime_id: String,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub event_head_seq: u64,
    pub oldest_retained_seq: u64,
    pub checked_turn_limit: usize,
    pub canonical: ConversationShadowMetadata,
    pub projection: ConversationShadowMetadata,
    pub legacy_unattributed_briefs: usize,
    pub mismatch_count: usize,
    pub mismatches: Vec<ConversationShadowMismatch>,
    pub mismatch_samples_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationShadowMetadata {
    pub turns: usize,
    pub active_turns: usize,
    pub pending_inputs: usize,
    pub briefs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationShadowMismatch {
    pub kind: ConversationShadowMismatchKind,
    pub entity_id: String,
    pub canonical_revision: Option<u64>,
    pub projection_revision: Option<u64>,
    pub canonical_count: Option<usize>,
    pub projection_count: Option<usize>,
    pub canonical_state: Option<PendingInputState>,
    pub projection_state: Option<PendingInputState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationShadowMismatchKind {
    MissingProjectionTurn,
    UnexpectedProjectionTurn,
    TurnRevision,
    BriefMembership,
    ActiveMembership,
    MissingProjectionInput,
    UnexpectedProjectionInput,
    InputRevision,
    InputState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_result: Option<TaskResultPresentation>,
    pub message_id: String,
    pub revision: u64,
    pub state: PendingInputState,
    pub preview: String,
    /// Operator display name snapshotted in the canonical message origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_display_name: Option<String>,
    pub presentation_class: PresentationClass,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputState {
    Queued,
    Assigning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionState {
    Active,
    Terminal { outcome: TerminalOutcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Completed,
    Aborted,
    Interrupted,
    BaselineOverBudget,
    DeferredToFallback,
    ProviderFailedNeedsRecovery,
}

impl From<TurnTerminalKind> for TerminalOutcome {
    fn from(value: TurnTerminalKind) -> Self {
        match value {
            TurnTerminalKind::Completed => Self::Completed,
            TurnTerminalKind::Aborted => Self::Aborted,
            TurnTerminalKind::Interrupted => Self::Interrupted,
            TurnTerminalKind::BaselineOverBudget => Self::BaselineOverBudget,
            TurnTerminalKind::DeferredToFallback => Self::DeferredToFallback,
            TurnTerminalKind::ProviderFailedNeedsRecovery => Self::ProviderFailedNeedsRecovery,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultState {
    Pending,
    Available,
    None {
        reason: NoBriefReason,
    },
    Unavailable {
        reason: ResultUnavailableReason,
        retryable: bool,
    },
    /// The turn's conversation content was deleted; no result body exists.
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoBriefReason {
    ReducerOnly { reason: String },
    Aborted,
    Interrupted,
    ToolOnlyWait,
}

impl From<&TurnNoBriefReason> for NoBriefReason {
    fn from(value: &TurnNoBriefReason) -> Self {
        match value {
            TurnNoBriefReason::ReducerOnly { reason } => Self::ReducerOnly {
                reason: reason.clone(),
            },
            TurnNoBriefReason::Aborted => Self::Aborted,
            TurnNoBriefReason::Interrupted => Self::Interrupted,
            TurnNoBriefReason::ToolOnlyWait => Self::ToolOnlyWait,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultUnavailableReason {
    MissingCanonicalLinkage,
    RetentionGap,
    LegacyCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attention {
    Failed { outcome: TerminalOutcome },
    Interrupted,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetailCoverage {
    Complete,
    Partial { reason: DetailCoverageReason },
    Unavailable { reason: DetailCoverageReason },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DetailCoverageReason {
    RetentionGap,
    LegacyOwnership,
    UnknownActivityType,
    MissingCanonicalLinkage,
    ContentDeleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PresentationClass {
    Operator,
    Task,
    External,
    Timer,
    Internal,
    System,
    Operational,
}

/// Derives immutable presentation membership only from the recorded creation trigger.
pub fn presentation_class(trigger: &TurnTriggerSummary) -> PresentationClass {
    message_presentation_class(&trigger.kind, trigger.trigger_kind)
}

/// Use the same canonical source classification before and after turn assignment.
pub fn message_presentation_class(
    kind: &MessageKind,
    trigger_kind: Option<ContinuationTriggerKind>,
) -> PresentationClass {
    match trigger_kind {
        Some(ContinuationTriggerKind::OperatorInput) => PresentationClass::Operator,
        Some(ContinuationTriggerKind::TaskResult) => PresentationClass::Task,
        Some(ContinuationTriggerKind::ExternalEvent) => PresentationClass::External,
        Some(ContinuationTriggerKind::TimerFire) => PresentationClass::Timer,
        Some(ContinuationTriggerKind::InternalFollowup) => PresentationClass::Internal,
        Some(ContinuationTriggerKind::SystemTick) => PresentationClass::System,
        None => match kind {
            MessageKind::OperatorPrompt => PresentationClass::Operator,
            MessageKind::TaskResult | MessageKind::TaskStatus => PresentationClass::Task,
            MessageKind::ChannelEvent | MessageKind::WebhookEvent | MessageKind::CallbackEvent => {
                PresentationClass::External
            }
            MessageKind::TimerTick => PresentationClass::Timer,
            MessageKind::InternalFollowup => PresentationClass::Internal,
            MessageKind::SystemTick => PresentationClass::System,
            MessageKind::Control | MessageKind::BriefAck | MessageKind::BriefResult => {
                PresentationClass::Operational
            }
        },
    }
}

/// Maps canonical result facts without inferring finality from terminal state or Brief presence.
pub fn map_result(
    brief_count: usize,
    no_brief_reason: Option<&TurnNoBriefReason>,
    canonical_settled: bool,
) -> (ResultState, bool) {
    if brief_count > 0 {
        return (ResultState::Available, canonical_settled);
    }
    if let Some(reason) = no_brief_reason {
        return (
            ResultState::None {
                reason: reason.into(),
            },
            true,
        );
    }
    if canonical_settled {
        return (
            ResultState::Unavailable {
                reason: ResultUnavailableReason::MissingCanonicalLinkage,
                retryable: false,
            },
            true,
        );
    }
    (ResultState::Pending, false)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationActivity {
    Operator(ActivityItem),
    Assistant(ActivityItem),
    Tool(ActivityItem),
    Wait(ActivityItem),
    Error(ActivityItem),
    ModelRequest(ModelRequestActivity),
    ModelSwitch(ModelSwitchActivity),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityItem {
    pub id: String,
    pub key: ActivityKey,
    pub revision: u64,
    pub summary: String,
}

/// Single authoritative registry of conversation activity source kinds.
///
/// Every source kind that can appear in `conversation_source_revisions` MUST be
/// listed here. Activity SQL, coverage checks, and deletion/retention
/// whitelists derive from this registry so a newly added kind cannot silently
/// drift out of one hardcoded list (the failure mode is an activity that
/// silently disappears or coverage that flips to `partial`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationSourceKind {
    Operator,
    Assistant,
    Tool,
    Wait,
    Error,
    ModelRequest,
    ModelSwitch,
}

impl ConversationSourceKind {
    pub const ALL: [Self; 7] = [
        Self::Operator,
        Self::Assistant,
        Self::Tool,
        Self::Wait,
        Self::Error,
        Self::ModelRequest,
        Self::ModelSwitch,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Wait => "wait",
            Self::Error => "error",
            Self::ModelRequest => "model_request",
            Self::ModelSwitch => "model_switch",
        }
    }

    /// Evidence table backing this activity's display summary.
    pub const fn evidence_table(self) -> &'static str {
        match self {
            Self::Operator => "messages",
            Self::Assistant | Self::Error => "transcript_entries",
            Self::Tool => "tool_executions",
            Self::Wait => "wait_conditions",
            Self::ModelRequest | Self::ModelSwitch => "model_requests",
        }
    }

    /// True when the kind's evidence lives in its own turn-content bearing
    /// table, so it participates in shared-content conflict checks before turn
    /// content deletion.
    pub const fn shares_exclusive_content_table(self) -> bool {
        matches!(
            self,
            Self::Operator | Self::Assistant | Self::Tool | Self::ModelRequest | Self::ModelSwitch
        )
    }

    pub const fn is_model(self) -> bool {
        matches!(self, Self::ModelRequest | Self::ModelSwitch)
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// Bounded, credential-free status of one model request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRequestStatus {
    InFlight,
    Succeeded,
    Failed,
    Aborted,
    /// Read-time projection of an unpaired `started` on a terminal turn. Never
    /// persisted and never backfilled with a synthetic `finished` fact.
    Unfinished,
}

impl ModelRequestStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InFlight => "in_flight",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Unfinished => "unfinished",
        }
    }
}

const MODEL_ACTIVITY_SUMMARY_MAX_CHARS: usize = 256;

fn bounded_summary(value: String) -> String {
    if value.chars().count() <= MODEL_ACTIVITY_SUMMARY_MAX_CHARS {
        value
    } else {
        value
            .chars()
            .take(MODEL_ACTIVITY_SUMMARY_MAX_CHARS)
            .collect()
    }
}

/// Builds the bounded display summary for a model request activity.
pub fn model_request_summary(
    provider: &str,
    model_ref: &str,
    status: ModelRequestStatus,
    duration_ms: Option<u64>,
    failure_kind: Option<&str>,
) -> String {
    let mut summary = format!("{provider}/{model_ref} · {}", status.as_str());
    if status == ModelRequestStatus::Failed {
        if let Some(kind) = failure_kind {
            summary.push_str(" · ");
            summary.push_str(kind);
        }
    }
    if let Some(duration) = duration_ms {
        summary.push_str(" · ");
        summary.push_str(&duration.to_string());
        summary.push_str("ms");
    }
    bounded_summary(summary)
}

/// Builds the bounded display summary for a model switch activity.
pub fn model_switch_summary(
    from_model_ref: &str,
    to_model_ref: &str,
    reason_category: &str,
) -> String {
    bounded_summary(format!(
        "{from_model_ref} -> {to_model_ref} · {reason_category}"
    ))
}

/// One projected model request inside the turn activity stream.
///
/// Field set is exactly the #4519 whitelist: no credentials, URLs, headers,
/// raw provider error bodies, or hidden reasoning may appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRequestActivity {
    pub id: String,
    pub key: ActivityKey,
    pub revision: u64,
    pub summary: String,
    pub provider: String,
    pub model_ref: String,
    pub dispatch_reason: String,
    pub status: ModelRequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
}

/// One projected model switch inside the turn activity stream. Immutable: the
/// switch is a single fact and never bumps its revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelSwitchActivity {
    pub id: String,
    pub key: ActivityKey,
    pub revision: u64,
    pub summary: String,
    pub from_provider: String,
    pub from_model_ref: String,
    pub to_provider: String,
    pub to_model_ref: String,
    pub reason_category: String,
}

/// Persisted payload of one `model_request` evidence row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequestEvidence {
    pub request_id: String,
    pub turn_id: String,
    pub provider: String,
    pub model_ref: String,
    pub dispatch_reason: String,
    pub status: ModelRequestStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
}

/// Persisted payload of one `model_switch` evidence row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSwitchEvidence {
    pub switch_id: String,
    pub turn_id: String,
    pub from_provider: String,
    pub from_model_ref: String,
    pub to_provider: String,
    pub to_model_ref: String,
    pub reason_category: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationActivityPage {
    pub turn: ConversationTurnSummary,
    pub detail_revision: u64,
    pub activities: Vec<ConversationActivity>,
    pub coverage: DetailCoverage,
    pub membership_upper_bound: Option<ActivityKey>,
    pub next_before: Option<ActivityKey>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationChange {
    OperatorUpsert {
        input: PendingInput,
    },
    OperatorRemove {
        message_id: String,
        revision: u64,
    },
    TurnSummaryUpsert {
        turn: ConversationTurnSummary,
    },
    ActivityUpsert {
        turn_id: String,
        activity: ConversationActivity,
    },
    DetailInvalidated {
        turn_id: String,
        detail_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBinding {
    pub runtime_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub agent_incarnation: u64,
    pub event_log_epoch: String,
    pub visibility_scope_id: String,
    pub schema_version: u32,
    pub query_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub binding: CursorBinding,
    pub before: TurnKey,
    pub membership_upper_bound: TurnKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryAfterCursor {
    pub binding: CursorBinding,
    pub after: TurnKey,
    pub membership_upper_bound: TurnKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetailCursor {
    pub binding: CursorBinding,
    pub turn_id: String,
    pub before: ActivityKey,
    pub membership_upper_bound: ActivityKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamCursor {
    pub binding: CursorBinding,
    pub event_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    History,
    HistoryAfter,
    Detail,
    Stream,
}

impl CursorKind {
    fn name(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::HistoryAfter => "history_after",
            Self::Detail => "detail",
            Self::Stream => "stream",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorDecodeError {
    Malformed,
    Tampered,
    KindMismatch,
    BindingMismatch,
    EventLogEpochMismatch,
    SchemaVersionMismatch { expected: u32, actual: u32 },
    QueryVersionMismatch { expected: u32, actual: u32 },
}

impl fmt::Display for CursorDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CursorDecodeError {}

#[derive(Debug, Serialize, Deserialize)]
struct CursorEnvelope<T> {
    kind: String,
    value: T,
}

#[derive(Debug, Deserialize)]
struct RawCursorEnvelope {
    kind: String,
    value: serde_json::Value,
}

pub trait ConversationCursor: Serialize + DeserializeOwned {
    const KIND: CursorKind;
    fn binding(&self) -> &CursorBinding;
}

impl ConversationCursor for HistoryCursor {
    const KIND: CursorKind = CursorKind::History;
    fn binding(&self) -> &CursorBinding {
        &self.binding
    }
}

impl ConversationCursor for HistoryAfterCursor {
    const KIND: CursorKind = CursorKind::HistoryAfter;
    fn binding(&self) -> &CursorBinding {
        &self.binding
    }
}

impl ConversationCursor for DetailCursor {
    const KIND: CursorKind = CursorKind::Detail;
    fn binding(&self) -> &CursorBinding {
        &self.binding
    }
}

impl ConversationCursor for StreamCursor {
    const KIND: CursorKind = CursorKind::Stream;
    fn binding(&self) -> &CursorBinding {
        &self.binding
    }
}

pub struct CursorCodec {
    signing_key: Vec<u8>,
}

impl CursorCodec {
    pub fn new(signing_key: impl Into<Vec<u8>>) -> Self {
        Self {
            signing_key: signing_key.into(),
        }
    }

    pub fn encode<T: ConversationCursor>(&self, cursor: &T) -> String {
        let payload = serde_json::to_vec(&CursorEnvelope {
            kind: T::KIND.name().to_owned(),
            value: cursor,
        })
        .expect("conversation cursor types are serializable");
        let signature = self.signature(&payload);
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(signature)
        )
    }

    pub fn decode<T: ConversationCursor>(
        &self,
        encoded: &str,
        expected_binding: &CursorBinding,
    ) -> Result<T, CursorDecodeError> {
        let (payload, signature) = encoded
            .split_once('.')
            .ok_or(CursorDecodeError::Malformed)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| CursorDecodeError::Malformed)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| CursorDecodeError::Malformed)?;
        if !constant_time_eq(&signature, &self.signature(&payload)) {
            return Err(CursorDecodeError::Tampered);
        }
        let envelope: RawCursorEnvelope =
            serde_json::from_slice(&payload).map_err(|_| CursorDecodeError::Malformed)?;
        if envelope.kind != T::KIND.name() {
            return Err(CursorDecodeError::KindMismatch);
        }
        let cursor: T =
            serde_json::from_value(envelope.value).map_err(|_| CursorDecodeError::Malformed)?;
        validate_binding(cursor.binding(), expected_binding)?;
        Ok(cursor)
    }

    fn signature(&self, payload: &[u8]) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"holon.conversation.cursor.v1\0");
        digest.update((self.signing_key.len() as u64).to_be_bytes());
        digest.update(&self.signing_key);
        digest.update((payload.len() as u64).to_be_bytes());
        digest.update(payload);
        digest.update(&self.signing_key);
        digest.finalize().into()
    }
}

fn validate_binding(
    actual: &CursorBinding,
    expected: &CursorBinding,
) -> Result<(), CursorDecodeError> {
    if actual.schema_version != expected.schema_version {
        return Err(CursorDecodeError::SchemaVersionMismatch {
            expected: expected.schema_version,
            actual: actual.schema_version,
        });
    }
    if actual.query_version != expected.query_version {
        return Err(CursorDecodeError::QueryVersionMismatch {
            expected: expected.query_version,
            actual: actual.query_version,
        });
    }
    if actual.event_log_epoch != expected.event_log_epoch {
        return Err(CursorDecodeError::EventLogEpochMismatch);
    }
    if actual.runtime_id != expected.runtime_id
        || actual.agent_id != expected.agent_id
        || actual.agent_incarnation != expected.agent_incarnation
        || actual.visibility_scope_id != expected.visibility_scope_id
    {
        return Err(CursorDecodeError::BindingMismatch);
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

#[cfg(test)]
mod tests;
