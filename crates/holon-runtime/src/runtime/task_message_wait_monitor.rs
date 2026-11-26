//! Convergence policy for the durable Agent Message wait monitor loops.
//!
//! Both the reply read and the result commit of a persisted wait task can fail.
//! Retrying is only meaningful while the failure can still change: an owner
//! that was replaced or a withdrawn Task record can never succeed, and the
//! previous unconditional one-second retry kept orphan monitors alive and
//! flooded the journal (#4601).

use std::time::Duration;

use tokio::time::Instant;

use crate::runtime_db::task_snapshot::TaskSnapshotError;
use crate::runtime_error::RuntimeError;

use super::mutation_owner::MutationOwnerError;

/// Pacing of the bounded retry used by the wait monitor loops. The first
/// attempt is immediate, then the delay doubles up to the cap until the budget
/// is spent. Both loops retain one recovery responsibility for a durable task,
/// so pacing only changes latency and journal volume, not durability.
const RETRY_MIN_DELAY: Duration = Duration::from_millis(500);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const RETRY_BUDGET: Duration = Duration::from_secs(15 * 60);

/// Why a monitor pass must not simply retry forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageWaitMonitorFailure {
    /// Another mutation owner superseded this instance's slot. Only the
    /// successor owner can resolve the durable wait, so this monitor stops and
    /// leaves the persisted task for owner reconciliation.
    OwnerReplaced,
    /// The Task record is gone; there is no durable target left to finish.
    TaskWithdrawn,
    /// The persisted Task no longer describes this wait (foreign record,
    /// changed canonical body, or retired scope).
    CanonicalChanged,
    /// The failure may still resolve; retry within the bounded budget.
    Retryable,
}

impl MessageWaitMonitorFailure {
    /// Classify a read or commit failure. Unrecognized failures stay retryable
    /// because the retry budget bounds them; only failures that provably cannot
    /// succeed here stop the monitor early.
    pub(super) fn classify(error: &anyhow::Error) -> Self {
        if error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<MutationOwnerError>(),
                Some(MutationOwnerError::Replaced)
            )
        }) {
            return Self::OwnerReplaced;
        }
        let runtime_code = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RuntimeError>())
            .map(|error| error.descriptor().code.as_str());
        if runtime_code == Some("task_not_found") {
            return Self::TaskWithdrawn;
        }
        if error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<TaskSnapshotError>(),
                Some(
                    TaskSnapshotError::CanonicalChanged(_)
                        | TaskSnapshotError::ForeignRecord(_)
                        | TaskSnapshotError::RetiredScope
                )
            )
        }) {
            return Self::CanonicalChanged;
        }
        Self::Retryable
    }
}

/// Bounded exponential retry used by both monitor loops.
#[derive(Debug)]
pub(super) struct MessageWaitMonitorRetry {
    next_delay: Duration,
    deadline: Instant,
}

impl MessageWaitMonitorRetry {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            next_delay: RETRY_MIN_DELAY,
            deadline: now + RETRY_BUDGET,
        }
    }

    /// Delay before the next attempt, or `None` once the budget is spent.
    pub(super) fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        if now >= self.deadline {
            return None;
        }
        let delay = self.next_delay.min(self.deadline - now);
        self.next_delay = (self.next_delay * 2).min(RETRY_MAX_DELAY);
        Some(delay)
    }
}

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaced_owner_stops_the_monitor() {
        let error = anyhow::Error::new(MutationOwnerError::Replaced);
        assert_eq!(
            MessageWaitMonitorFailure::classify(&error),
            MessageWaitMonitorFailure::OwnerReplaced
        );
        let contexted = error.context("message wait result commit");
        assert_eq!(
            MessageWaitMonitorFailure::classify(&contexted),
            MessageWaitMonitorFailure::OwnerReplaced
        );
    }

    #[test]
    fn withdrawn_task_record_stops_the_monitor() {
        let error: anyhow::Error = RuntimeError::not_found("task_not_found", "task gone").into();
        assert_eq!(
            MessageWaitMonitorFailure::classify(&error),
            MessageWaitMonitorFailure::TaskWithdrawn
        );
    }

    #[test]
    fn canonical_change_stops_the_monitor() {
        let error: anyhow::Error = TaskSnapshotError::RetiredScope.into();
        assert_eq!(
            MessageWaitMonitorFailure::classify(&error),
            MessageWaitMonitorFailure::CanonicalChanged
        );
    }

    #[test]
    fn transient_and_unknown_failures_stay_retryable() {
        let busy = anyhow::Error::new(MutationOwnerError::Busy);
        assert_eq!(
            MessageWaitMonitorFailure::classify(&busy),
            MessageWaitMonitorFailure::Retryable
        );
        let unknown = anyhow::anyhow!("message wait outcome belongs to another route");
        assert_eq!(
            MessageWaitMonitorFailure::classify(&unknown),
            MessageWaitMonitorFailure::Retryable
        );
        let preparation: anyhow::Error = TaskSnapshotError::CoverageNeedsPreparation.into();
        assert_eq!(
            MessageWaitMonitorFailure::classify(&preparation),
            MessageWaitMonitorFailure::Retryable
        );
    }

    #[test]
    fn retry_backoff_grows_caps_and_then_expires() {
        let start = Instant::now();
        let mut retry = MessageWaitMonitorRetry::new(start);
        let mut delays = Vec::new();
        let mut now = start;
        while let Some(delay) = retry.next_delay(now) {
            delays.push(delay);
            now += delay;
            assert!(delays.len() < 10_000, "retry budget must expire");
        }
        assert_eq!(delays[0], RETRY_MIN_DELAY);
        assert_eq!(delays[1], RETRY_MIN_DELAY * 2);
        assert!(delays.iter().all(|delay| *delay <= RETRY_MAX_DELAY));
        assert!(delays.iter().any(|delay| *delay == RETRY_MAX_DELAY));
        assert!(now >= start + RETRY_BUDGET);
        assert_eq!(retry.next_delay(now), None);
    }
}
