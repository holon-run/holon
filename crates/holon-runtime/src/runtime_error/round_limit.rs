//! Typed rejection for provider round admission limits.
//!
//! The admission check itself lives in `crate::runtime::turn::round_admission`;
//! this module only owns the typed error so `describe_runtime_error` can
//! classify it without reaching into the private turn module.

/// Stable failure reason code for a provider response that exceeded a round
/// admission quota.
pub(crate) const PROVIDER_ROUND_LIMIT_EXCEEDED_CODE: &str = "provider_round_limit_exceeded";

/// Which round admission quota was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderRoundLimitQuota {
    ManagedToolCalls,
    ToolCallInputBytes,
    RoundVisiblePayloadBytes,
    ExpandedWorkItems,
    BatchItems,
}

impl ProviderRoundLimitQuota {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ManagedToolCalls => "managed_tool_calls",
            Self::ToolCallInputBytes => "tool_call_input_bytes",
            Self::RoundVisiblePayloadBytes => "round_visible_payload_bytes",
            Self::ExpandedWorkItems => "expanded_work_items",
            Self::BatchItems => "batch_items",
        }
    }
}

/// Typed rejection carried into the runtime failure terminal path. Contains
/// only trusted enums and bounded numeric evidence; it never echoes tool
/// names, parameter bodies, or response text.
#[derive(Debug, Clone)]
pub(crate) struct ProviderRoundLimitExceeded {
    pub(crate) quota: ProviderRoundLimitQuota,
    pub(crate) round: usize,
    pub(crate) limit: u64,
    /// Observed usage. A lower bound when `count_is_lower_bound` is set
    /// because byte accounting stopped at the budget.
    pub(crate) observed: u64,
    pub(crate) count_is_lower_bound: bool,
    pub(crate) managed_tool_calls: usize,
}

impl std::fmt::Display for ProviderRoundLimitExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "provider round exceeded admission limit (quota={}, limit={}, observed={}, count={}, round={}, managed_tool_calls={})",
            self.quota.as_str(),
            self.limit,
            self.observed,
            if self.count_is_lower_bound {
                "at_least"
            } else {
                "complete"
            },
            self.round,
            self.managed_tool_calls,
        )
    }
}

impl std::error::Error for ProviderRoundLimitExceeded {}
