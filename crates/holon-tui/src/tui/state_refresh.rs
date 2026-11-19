use std::time::{Duration, Instant};

const HINT_WINDOW: Duration = Duration::from_millis(300);
const MIN_AUTOMATIC_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Owns refresh timing only; the caller owns I/O and selection-scope admission.
#[derive(Clone, Default)]
pub(super) struct StateRefreshScheduler {
    in_flight: bool,
    hint_deadline: Option<Instant>,
    explicit_deadline: Option<Instant>,
    last_started_at: Option<Instant>,
    last_settled_at: Option<Instant>,
    retry_not_before: Option<Instant>,
    retry_delay: Duration,
}

impl StateRefreshScheduler {
    pub(super) fn hint(&mut self, now: Instant) {
        // Keep the first deadline so a continuous hint stream cannot starve reads.
        self.hint_deadline.get_or_insert(now + HINT_WINDOW);
    }

    pub(super) fn request(&mut self, now: Instant) {
        self.explicit_deadline.get_or_insert(now);
    }

    /// `active` selects the polling cadence; idle agents still receive polls.
    pub(super) fn is_due(&self, now: Instant, active: bool) -> bool {
        if self.in_flight || self.retry_not_before.is_some_and(|deadline| now < deadline) {
            return false;
        }
        if let Some(deadline) = self.explicit_deadline {
            return now >= deadline;
        }
        if self
            .last_started_at
            .is_some_and(|started| now.saturating_duration_since(started) < MIN_AUTOMATIC_INTERVAL)
        {
            return false;
        }

        let poll_interval = if active {
            MIN_AUTOMATIC_INTERVAL
        } else {
            IDLE_POLL_INTERVAL
        };
        self.hint_deadline.is_some_and(|deadline| now >= deadline)
            || self
                .retry_not_before
                .is_some_and(|deadline| now >= deadline)
            || self
                .last_settled_at
                .is_some_and(|settled| now.saturating_duration_since(settled) >= poll_interval)
    }

    /// Record every actual read, including initial and selection bootstrap reads.
    pub(super) fn started(&mut self, now: Instant) {
        self.in_flight = true;
        self.last_started_at = Some(now);
        // Consume only work preceding this read; in-flight hints remain pending.
        self.hint_deadline = None;
        self.explicit_deadline = None;
    }

    pub(super) fn settled(&mut self, now: Instant, success: bool) {
        self.in_flight = false;
        self.last_settled_at = Some(now);
        if success {
            self.retry_delay = Duration::ZERO;
            self.retry_not_before = None;
        } else {
            self.retry_delay = if self.retry_delay.is_zero() {
                MIN_AUTOMATIC_INTERVAL
            } else {
                (self.retry_delay * 2).min(MAX_RETRY_DELAY)
            };
            // Both automatic and explicit work must respect failure backoff.
            self.retry_not_before = Some(now + self.retry_delay);
        }
    }
}

#[cfg(test)]
mod tests;
