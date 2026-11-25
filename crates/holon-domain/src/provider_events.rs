use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Fact source for `provider_switch`: the runtime switched the model route it
/// dispatches to inside one turn.
///
/// The payload is deliberately whitelisted: no raw provider error text, URL,
/// header, credential, or diagnostics identifier is carried. The only reason
/// information is the coarse `failure_kind` category of the attempt that
/// triggered the switch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSwitchEvent {
    pub turn_id: String,
    pub turn_index: u64,
    pub round: usize,
    /// 1-based ordinal of this switch inside the turn.
    pub switch_ordinal: u64,
    pub from_provider: String,
    pub from_model_ref: String,
    pub to_provider: String,
    pub to_model_ref: String,
    pub reason_category: ProviderSwitchReason,
    pub occurred_at: DateTime<Utc>,
}

/// Coarse, credential-free category for a model switch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSwitchReason {
    Quota,
    RateLimited,
    Auth,
    Timeout,
    Unavailable,
    Unknown,
}

impl ProviderSwitchReason {
    /// Maps a typed provider `failure_kind` into the public category set.
    /// Unknown or missing kinds stay `unknown`; the switch itself is still
    /// reported rather than hidden.
    pub fn from_failure_kind(failure_kind: Option<&str>) -> Self {
        match failure_kind {
            Some("rate_limited") => Self::RateLimited,
            Some("payment_required") => Self::Quota,
            Some("auth_error") => Self::Auth,
            Some("timeout") => Self::Timeout,
            Some(
                "connection"
                | "server_error"
                | "empty_response"
                | "credential_refresh_busy"
                | "unsupported_transport",
            ) => Self::Unavailable,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quota => "quota",
            Self::RateLimited => "rate_limited",
            Self::Auth => "auth",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

/// Failure kinds the runtime may publish on `provider_request_finished`.
///
/// These are the typed `ProviderFailureKind` wire values. The event is part of
/// the public typed matrix, so it must never carry a raw provider error code,
/// URL, header, or free-form diagnostics string.
pub const PUBLIC_PROVIDER_FAILURE_KINDS: &[&str] = &[
    "timeout",
    "connection",
    "credential_refresh_busy",
    "rate_limited",
    "payment_required",
    "server_error",
    "empty_response",
    "auth_error",
    "contract_error",
    "invalid_response",
    "unsupported_transport",
    "unknown",
];

/// `provider_request_started`: one model request is about to be dispatched.
///
/// `request_id` is the stable pairing key with
/// [`ProviderRequestFinishedEvent`]; a started request without a matching
/// finished event is a legal terminal state (process crash, pre-submit abort)
/// and must never be backfilled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRequestStartedEvent {
    pub request_id: String,
    pub turn_id: String,
    pub turn_index: u64,
    pub round: usize,
    pub provider: String,
    /// Route the runtime resolved for this dispatch. It does not claim the
    /// final endpoint a provider transport may pick internally.
    pub model_ref: String,
    pub started_at: DateTime<Utc>,
    pub dispatch_reason: ProviderDispatchReason,
}

/// Why the runtime dispatched this request again inside the same round.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDispatchReason {
    Initial,
    AfterSwitch,
    AfterInterjection,
    Retry,
}

impl ProviderDispatchReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::AfterSwitch => "after_switch",
            Self::AfterInterjection => "after_interjection",
            Self::Retry => "retry",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRequestOutcome {
    Succeeded,
    Failed,
    Aborted,
}

impl ProviderRequestOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }
}

/// `provider_request_finished`: the dispatch started by the matching
/// `request_id` reached a terminal outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRequestFinishedEvent {
    pub request_id: String,
    pub outcome: ProviderRequestOutcome,
    pub provider: String,
    /// Actual route used by the dispatch when known (`winning_model_ref` /
    /// `active_model_ref`); otherwise the started route with
    /// `route_known = false`.
    pub model_ref: String,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    pub route_known: bool,
}
