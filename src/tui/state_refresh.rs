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
mod tests {
    use super::*;

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn untouched_scheduler_does_not_start_polling() {
        let scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        for active in [false, true] {
            assert!(!scheduler.is_due(now, active));
            assert!(!scheduler.is_due(after(now, 3_600_000), active));
        }
    }

    #[test]
    fn burst_hints_keep_the_first_deadline_and_coalesce_one_read() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.hint(now);
        scheduler.hint(after(now, 100));
        scheduler.hint(after(now, 299));
        assert!(!scheduler.is_due(after(now, 299), false));
        scheduler.hint(after(now, 300));
        assert!(scheduler.is_due(after(now, 300), false));

        scheduler.started(after(now, 300));
        scheduler.settled(after(now, 400), true);
        assert!(!scheduler.is_due(after(now, 5_300), false));
        assert!(!scheduler.is_due(after(now, 30_399), false));
        assert!(scheduler.is_due(after(now, 30_400), false));
    }

    #[test]
    fn continuous_hints_do_not_push_the_deadline_back() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.hint(now);
        for millis in (100..=10_000).step_by(100) {
            scheduler.hint(after(now, millis));
            assert_eq!(scheduler.is_due(after(now, millis), false), millis >= 300);
        }
    }

    #[test]
    fn automatic_reads_start_at_least_five_seconds_apart() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.settled(after(now, 100), true);
        scheduler.hint(after(now, 200));
        assert!(!scheduler.is_due(after(now, 4_999), true));
        assert!(scheduler.is_due(after(now, 5_000), true));

        scheduler.started(after(now, 5_000));
        scheduler.settled(after(now, 5_100), true);
        scheduler.hint(after(now, 5_200));
        assert!(!scheduler.is_due(after(now, 9_999), false));
        assert!(scheduler.is_due(after(now, 10_000), false));
    }

    #[test]
    fn in_flight_hints_survive_settlement_and_coalesce_one_followup() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.hint(after(now, 1_000));
        scheduler.hint(after(now, 2_000));
        assert!(!scheduler.is_due(after(now, 6_000), true));
        scheduler.settled(after(now, 6_000), true);
        assert!(scheduler.is_due(after(now, 6_000), false));

        scheduler.started(after(now, 6_000));
        scheduler.settled(after(now, 6_100), true);
        assert!(!scheduler.is_due(after(now, 11_000), false));
        assert!(scheduler.is_due(after(now, 36_100), false));
    }

    #[test]
    fn in_flight_hint_keeps_its_original_coalescing_window() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.hint(after(now, 7_000));
        scheduler.settled(after(now, 7_100), true);
        assert!(!scheduler.is_due(after(now, 7_299), false));
        assert!(scheduler.is_due(after(now, 7_300), false));
    }

    #[test]
    fn unchanged_success_without_a_hint_polls_from_settlement() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.settled(after(now, 4_000), true);
        assert!(!scheduler.is_due(after(now, 5_000), true));
        assert!(!scheduler.is_due(after(now, 8_999), true));
        assert!(scheduler.is_due(after(now, 9_000), true));

        assert!(!scheduler.is_due(after(now, 33_999), false));
        assert!(scheduler.is_due(after(now, 34_000), false));
    }

    #[test]
    fn active_state_changes_recompute_the_poll_interval() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.settled(after(now, 100), true);
        assert!(scheduler.is_due(after(now, 5_100), true));
        assert!(!scheduler.is_due(after(now, 5_100), false));
        assert!(scheduler.is_due(after(now, 30_100), false));
        assert!(scheduler.is_due(after(now, 30_100), true));
    }

    #[test]
    fn first_explicit_request_is_immediate() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.request(now);
        assert!(scheduler.is_due(now, false));
        scheduler.started(now);
        assert!(!scheduler.is_due(after(now, 30_000), true));
    }

    #[test]
    fn explicit_request_bypasses_automatic_spacing_and_consumes_prior_hint() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.settled(after(now, 100), true);
        scheduler.hint(after(now, 150));
        scheduler.request(after(now, 200));
        assert!(scheduler.is_due(after(now, 200), false));

        scheduler.started(after(now, 200));
        scheduler.settled(after(now, 300), true);
        assert!(!scheduler.is_due(after(now, 5_200), false));
    }

    #[test]
    fn explicit_requests_during_a_read_coalesce_one_immediate_followup() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.request(after(now, 100));
        scheduler.request(after(now, 200));
        assert!(!scheduler.is_due(after(now, 300), false));
        scheduler.settled(after(now, 400), true);
        assert!(scheduler.is_due(after(now, 400), false));

        scheduler.started(after(now, 400));
        scheduler.settled(after(now, 500), true);
        assert!(!scheduler.is_due(after(now, 600), true));
    }

    #[test]
    fn failed_reads_retry_without_hints_with_bounded_exponential_backoff() {
        let mut scheduler = StateRefreshScheduler::default();
        let mut now = Instant::now();

        for delay_ms in [5_000, 10_000, 20_000, 30_000, 30_000, 30_000] {
            scheduler.started(now);
            let settled = after(now, 100);
            scheduler.settled(settled, false);
            for active in [false, true] {
                assert!(!scheduler.is_due(after(settled, delay_ms - 1), active));
                assert!(scheduler.is_due(after(settled, delay_ms), active));
            }
            now = after(settled, delay_ms);
        }
    }

    #[test]
    fn hints_and_explicit_requests_cannot_bypass_failure_backoff() {
        for explicit in [false, true] {
            let mut scheduler = StateRefreshScheduler::default();
            let now = Instant::now();

            scheduler.started(now);
            scheduler.settled(after(now, 100), false);
            scheduler.started(after(now, 5_100));
            scheduler.settled(after(now, 5_200), false);
            for millis in [5_300, 10_100, 15_199] {
                if explicit {
                    scheduler.request(after(now, millis));
                } else {
                    scheduler.hint(after(now, millis));
                }
                assert!(!scheduler.is_due(after(now, millis), true));
            }
            assert!(scheduler.is_due(after(now, 15_200), false));
        }
    }

    #[test]
    fn explicit_request_during_a_failed_read_waits_for_retry() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.request(after(now, 100));
        scheduler.settled(after(now, 200), false);
        assert!(!scheduler.is_due(after(now, 5_199), true));
        assert!(scheduler.is_due(after(now, 5_200), false));

        scheduler.started(after(now, 5_200));
        scheduler.settled(after(now, 5_300), true);
        assert!(!scheduler.is_due(after(now, 5_400), true));
    }

    #[test]
    fn success_clears_backoff_for_explicit_requests_and_later_failures() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.settled(after(now, 100), false);
        scheduler.started(after(now, 5_100));
        scheduler.settled(after(now, 5_200), false);
        scheduler.started(after(now, 15_200));
        scheduler.settled(after(now, 15_300), true);
        scheduler.request(after(now, 15_400));
        assert!(scheduler.is_due(after(now, 15_400), false));

        scheduler.started(after(now, 15_400));
        scheduler.settled(after(now, 15_500), false);
        assert!(!scheduler.is_due(after(now, 20_499), true));
        assert!(scheduler.is_due(after(now, 20_500), false));
    }

    #[test]
    fn default_reset_drops_selection_scope_timing_and_pending_work() {
        let mut scheduler = StateRefreshScheduler::default();
        let now = Instant::now();

        scheduler.started(now);
        scheduler.settled(after(now, 100), false);
        scheduler.request(after(now, 200));
        scheduler.hint(after(now, 300));
        scheduler = StateRefreshScheduler::default();
        assert!(!scheduler.is_due(after(now, 3_600_000), true));
        scheduler.hint(after(now, 400));
        assert!(!scheduler.is_due(after(now, 699), false));
        assert!(scheduler.is_due(after(now, 700), false));

        scheduler.started(after(now, 700));
        scheduler.hint(after(now, 800));
        scheduler.request(after(now, 900));
        scheduler = StateRefreshScheduler::default();
        assert!(!scheduler.is_due(after(now, 3_600_000), false));
        scheduler.request(after(now, 1_000));
        assert!(scheduler.is_due(after(now, 1_000), true));
    }
}
