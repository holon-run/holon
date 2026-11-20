use std::sync::Mutex;

use anyhow::{anyhow, ensure, Result};
use tokio::sync::watch;

/// Startup bootstraps may progress while ordinary dispatch waits for the batch.
#[derive(Default)]
pub(super) struct StartupDispatch {
    current: Mutex<Option<watch::Sender<Option<bool>>>>,
}

impl StartupDispatch {
    pub(super) fn begin(&self) -> Result<StartupDispatchCompletion> {
        let mut current = self
            .current
            .lock()
            .map_err(|_| anyhow!("startup dispatch gate poisoned"))?;
        ensure!(
            !current
                .as_ref()
                .is_some_and(|sender| sender.borrow().is_none()),
            "startup recovery already in progress"
        );
        let (outcome, _) = watch::channel(None);
        *current = Some(outcome.clone());
        Ok(StartupDispatchCompletion {
            outcome,
            finished: false,
        })
    }

    pub(super) async fn wait(&self) -> Result<bool> {
        let receiver = self
            .current
            .lock()
            .map_err(|_| anyhow!("startup dispatch gate poisoned"))?
            .as_ref()
            .map(watch::Sender::subscribe);
        let Some(mut receiver) = receiver else {
            return Ok(true);
        };
        let outcome = *receiver.wait_for(Option::is_some).await?;
        Ok(outcome == Some(true))
    }
}

#[must_use = "release dispatch only after the remaining startup initialization succeeds"]
pub struct StartupDispatchCompletion {
    outcome: watch::Sender<Option<bool>>,
    finished: bool,
}

impl StartupDispatchCompletion {
    /// Allow ordinary dispatch after all startup initialization succeeds.
    pub fn release(self) {
        self.finish(true);
    }

    pub(super) fn finish(mut self, success: bool) {
        self.finished = true;
        self.outcome.send_replace(Some(success));
    }
}

impl Drop for StartupDispatchCompletion {
    fn drop(&mut self) {
        if !self.finished {
            self.outcome.send_replace(Some(false));
        }
    }
}

#[cfg(all(test, holon_lib_test_shard = "d"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abandoned_startup_closes_its_waiters_and_cannot_adopt_a_later_batch() -> Result<()> {
        let gate = StartupDispatch::default();
        assert!(gate.wait().await?);
        let first = gate.begin()?;
        assert!(gate.begin().is_err());
        let wait = gate.wait();
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        drop(first);
        let second = gate.begin()?;
        second.finish(true);
        assert!(!wait.await?, "the first wait retains its failed batch");
        assert!(gate.wait().await?);
        Ok(())
    }
}
