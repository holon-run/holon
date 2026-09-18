//! Pure domain types for the conversation read model.

use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::types::{
    ContinuationTriggerKind, MessageKind, TurnNoBriefReason, TurnTerminalKind, TurnTriggerSummary,
};

pub const CONVERSATION_SCHEMA_VERSION: u32 = 1;
pub const CONVERSATION_QUERY_VERSION: u32 = 1;

/// Canonical operator input attached to a turn, for summary-level rendering
/// without loading turn activity detail. Bounded per turn by the read model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnInputSummary {
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityItem {
    pub id: String,
    pub key: ActivityKey,
    pub revision: u64,
    pub summary: String,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct TurnKey {
    pub turn_index: u64,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct ActivityKey {
    pub event_seq: u64,
    pub activity_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorBinding {
    pub runtime_id: String,
    pub agent_id: String,
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
    Detail,
    Stream,
}

impl CursorKind {
    fn name(self) -> &'static str {
        match self {
            Self::History => "history",
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
mod tests {
    use super::*;
    use crate::types::{AuthorityClass, MessageOrigin, Priority};

    fn binding() -> CursorBinding {
        CursorBinding {
            runtime_id: "runtime".into(),
            agent_id: "agent".into(),
            event_log_epoch: "epoch".into(),
            visibility_scope_id: "scope".into(),
            schema_version: CONVERSATION_SCHEMA_VERSION,
            query_version: CONVERSATION_QUERY_VERSION,
        }
    }

    fn trigger(
        kind: MessageKind,
        trigger_kind: Option<ContinuationTriggerKind>,
    ) -> TurnTriggerSummary {
        TurnTriggerSummary {
            message_id: Some("message".into()),
            kind,
            origin: MessageOrigin::System {
                subsystem: "test".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            priority: Priority::Normal,
            trigger_kind,
            task_id: None,
        }
    }

    #[test]
    fn presentation_is_derived_from_creation_trigger() {
        assert_eq!(
            presentation_class(&trigger(
                MessageKind::InternalFollowup,
                Some(ContinuationTriggerKind::TaskResult),
            )),
            PresentationClass::Task
        );
        assert_eq!(
            presentation_class(&trigger(MessageKind::OperatorPrompt, None)),
            PresentationClass::Operator
        );
    }

    #[test]
    fn result_mapping_does_not_infer_settled() {
        assert_eq!(map_result(0, None, false), (ResultState::Pending, false));
        assert_eq!(
            map_result(0, None, true),
            (
                ResultState::Unavailable {
                    reason: ResultUnavailableReason::MissingCanonicalLinkage,
                    retryable: false,
                },
                true
            )
        );
        assert_eq!(map_result(2, None, false), (ResultState::Available, false));
        assert_eq!(map_result(2, None, true), (ResultState::Available, true));
        assert_eq!(
            map_result(0, Some(&TurnNoBriefReason::ToolOnlyWait), false),
            (
                ResultState::None {
                    reason: NoBriefReason::ToolOnlyWait
                },
                true
            )
        );
    }

    #[test]
    fn cursor_round_trips_and_is_url_safe() {
        let cursor = HistoryCursor {
            binding: binding(),
            before: TurnKey {
                turn_index: 10,
                turn_id: "turn-10".into(),
            },
            membership_upper_bound: TurnKey {
                turn_index: 20,
                turn_id: "turn-20".into(),
            },
        };
        let codec = CursorCodec::new(b"secret".to_vec());
        let encoded = codec.encode(&cursor);
        assert!(!encoded.contains(['+', '/', '=']));
        assert_eq!(codec.decode(&encoded, &binding()), Ok(cursor));
    }

    #[test]
    fn cursor_rejects_kind_binding_version_and_tampering() {
        let cursor = StreamCursor {
            binding: binding(),
            event_seq: 42,
        };
        let codec = CursorCodec::new(b"secret".to_vec());
        let encoded = codec.encode(&cursor);

        assert_eq!(
            codec.decode::<HistoryCursor>(&encoded, &binding()),
            Err(CursorDecodeError::KindMismatch)
        );

        let mut other = binding();
        other.agent_id = "other".into();
        assert_eq!(
            codec.decode::<StreamCursor>(&encoded, &other),
            Err(CursorDecodeError::BindingMismatch)
        );

        for other in [
            CursorBinding {
                runtime_id: "other".into(),
                ..binding()
            },
            CursorBinding {
                visibility_scope_id: "other".into(),
                ..binding()
            },
        ] {
            assert_eq!(
                codec.decode::<StreamCursor>(&encoded, &other),
                Err(CursorDecodeError::BindingMismatch)
            );
        }

        let mut other = binding();
        other.event_log_epoch = "other".into();
        assert_eq!(
            codec.decode::<StreamCursor>(&encoded, &other),
            Err(CursorDecodeError::EventLogEpochMismatch)
        );

        let mut other = binding();
        other.schema_version += 1;
        assert_eq!(
            codec.decode::<StreamCursor>(&encoded, &other),
            Err(CursorDecodeError::SchemaVersionMismatch {
                expected: 2,
                actual: 1
            })
        );

        let mut other = binding();
        other.query_version += 1;
        assert_eq!(
            codec.decode::<StreamCursor>(&encoded, &other),
            Err(CursorDecodeError::QueryVersionMismatch {
                expected: 2,
                actual: 1
            })
        );

        let mut tampered = encoded.into_bytes();
        tampered[0] = if tampered[0] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            codec.decode::<StreamCursor>(
                std::str::from_utf8(&tampered).expect("ASCII cursor"),
                &binding()
            ),
            Err(CursorDecodeError::Tampered)
        );
    }
}
