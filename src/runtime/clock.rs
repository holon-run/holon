use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin, time::Duration};

const WALL_CLOCK_RECHECK_MAX: Duration = Duration::from_secs(30);

pub(crate) type SleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    fn sleep_until(&self, deadline: DateTime<Utc>) -> SleepFuture<'_> {
        Box::pin(async move {
            loop {
                let now = self.now();
                if now >= deadline {
                    return;
                }
                let remaining = (deadline - now)
                    .to_std()
                    .expect("future UTC deadline must have a positive duration");
                tokio::time::sleep(remaining.min(WALL_CLOCK_RECHECK_MAX)).await;
            }
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
#[derive(Debug)]
/// Test wall clock whose inherited `sleep_until` uses Tokio time.
///
/// Sleep tests normally pause and advance Tokio alongside this clock.
/// Suspend and wall-clock-change regressions deliberately advance them separately.
pub(crate) struct TestClock {
    now: std::sync::Mutex<DateTime<Utc>>,
}

#[cfg(test)]
impl TestClock {
    pub(crate) fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: std::sync::Mutex::new(now),
        }
    }

    pub(crate) fn advance(&self, duration: std::time::Duration) {
        let duration = chrono::Duration::from_std(duration)
            .expect("test clock duration must fit chrono::Duration");
        let mut now = self.now.lock().expect("test clock lock poisoned");
        *now = now
            .checked_add_signed(duration)
            .expect("test clock advance must remain representable");
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("test clock lock poisoned")
    }
}

#[cfg(test)]
impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        self.now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    fn clock() -> TestClock {
        TestClock::new(DateTime::from_timestamp(1_700_000_000, 0).unwrap())
    }

    // Poll explicitly so paused Tokio cannot skip ahead to a stale deadline.
    fn poll_sleep(sleep: &mut (impl Future<Output = ()> + Unpin)) -> Poll<()> {
        Pin::new(sleep).poll(&mut Context::from_waker(Waker::noop()))
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_rechecks_wall_clock_after_suspend() {
        let clock = clock();
        let mut sleep = clock.sleep_until(clock.now() + chrono::Duration::hours(24));
        assert!(poll_sleep(&mut sleep).is_pending());

        clock.advance(Duration::from_secs(3 * 24 * 60 * 60));
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(poll_sleep(&mut sleep).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(poll_sleep(&mut sleep).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_does_not_finish_after_wall_clock_moves_backward() {
        let clock = clock();
        let start = clock.now();
        let mut sleep = clock.sleep_until(start + chrono::Duration::seconds(5));
        assert!(poll_sleep(&mut sleep).is_pending());

        *clock.now.lock().unwrap() = start - chrono::Duration::seconds(10);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(poll_sleep(&mut sleep).is_pending());

        clock.advance(Duration::from_secs(14));
        tokio::time::advance(Duration::from_secs(14)).await;
        assert!(poll_sleep(&mut sleep).is_pending());
        clock.advance(Duration::from_secs(1));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(poll_sleep(&mut sleep).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_preserves_short_deadlines() {
        let clock = clock();
        let mut sleep = clock.sleep_until(clock.now() + chrono::Duration::milliseconds(10));
        assert!(poll_sleep(&mut sleep).is_pending());
        clock.advance(Duration::from_millis(9));
        tokio::time::advance(Duration::from_millis(9)).await;
        assert!(poll_sleep(&mut sleep).is_pending());
        clock.advance(Duration::from_millis(1));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(poll_sleep(&mut sleep).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_preserves_long_deadlines() {
        let clock = clock();
        let mut sleep = clock.sleep_until(clock.now() + chrono::Duration::seconds(120));
        assert!(poll_sleep(&mut sleep).is_pending());
        for step in 1..=4 {
            clock.advance(Duration::from_secs(30));
            tokio::time::advance(Duration::from_secs(30)).await;
            assert_eq!(poll_sleep(&mut sleep).is_ready(), step == 4);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_finishes_immediately_for_due_and_overdue_deadlines() {
        let clock = clock();
        for deadline in [clock.now(), clock.now() - chrono::Duration::days(1)] {
            assert!(poll_sleep(&mut clock.sleep_until(deadline)).is_ready());
        }
    }
}
