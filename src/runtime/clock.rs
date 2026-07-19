use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin, time::Duration};

pub(crate) type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    fn sleep_until(&self, deadline: DateTime<Utc>) -> SleepFuture {
        let duration = (deadline - self.now()).to_std().unwrap_or(Duration::ZERO);
        Box::pin(tokio::time::sleep(duration))
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
