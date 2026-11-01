//! A short readiness fence, never an inventory or I/O lock.
use anyhow::{anyhow, ensure, Result};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::sync::Notify;

#[derive(Default)]
pub(crate) struct ContextResources {
    state: Mutex<State>,
    pub(super) drained: Notify,
    #[cfg(test)]
    pub(super) physical_checkpoint:
        Mutex<Option<Arc<super::resource_writes::PhysicalWriteBarrier>>>,
}
#[derive(Default)]
pub(super) struct State {
    pub generation: u64,
    pub active: usize,
    pub closing: bool,
    pub failed: bool,
    subscribers: Vec<Weak<Notify>>,
}
pub(crate) struct ReadyFence<'a> {
    _state: MutexGuard<'a, State>,
}

#[derive(Debug, thiserror::Error)]
#[error("context resources have active writers or are Closing")]
pub(crate) struct ContextResourceWritersActive;

impl ContextResources {
    pub(super) fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow!("resource fence poisoned"))
    }
    pub(crate) fn subscribe(&self, notify: &Arc<Notify>) -> Result<()> {
        let mut state = self.lock()?;
        state.subscribers.retain(|entry| entry.strong_count() > 0);
        state.subscribers.push(Arc::downgrade(notify));
        Ok(())
    }
    pub(crate) fn generation(&self) -> Result<u64> {
        let state = self.lock()?;
        ensure!(
            !state.closing,
            "context resources have active writers or are Closing"
        );
        ensure!(state.active == 0, ContextResourceWritersActive);
        Ok(state.generation)
    }
    pub(crate) fn ready(&self, generation: u64) -> Result<ReadyFence<'_>> {
        let state = self.lock()?;
        ensure!(
            !state.closing,
            "context resource Host generation changed or is not ready"
        );
        ensure!(state.active == 0, ContextResourceWritersActive);
        ensure!(
            state.generation == generation,
            "context resource Host generation changed or is not ready"
        );
        Ok(ReadyFence { _state: state })
    }
    pub(super) fn changed(state: &mut State) -> Result<()> {
        state.generation = state.generation.checked_add(1).ok_or_else(|| {
            state.closing = true;
            anyhow!("context resource Host generation exhausted")
        })?;
        for notify in state.subscribers.iter().filter_map(Weak::upgrade) {
            notify.notify_one();
        }
        Ok(())
    }
    pub(crate) fn invalidate(&self) -> Result<()> {
        let mut state = self.lock()?;
        Self::changed(&mut state)
    }
    /// Synchronous config publication shares the install fence; no I/O/await.
    pub(crate) fn publish_config<T>(&self, publish: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut state = self.lock()?;
        ensure!(!state.closing, "context resource configuration is Closing");
        Self::changed(&mut state)?;
        publish()
    }
    pub(super) fn begin_close(&self) -> Result<()> {
        let mut state = self.lock()?;
        state.closing = true;
        Self::changed(&mut state)
    }
    pub(crate) async fn wait_for_writers(&self) -> Result<()> {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let drained = {
                let state = self.lock()?;
                ensure!(!state.closing, "context resource preparation is Closing");
                ensure!(!state.failed, "physical context resource writer panicked");
                state.active == 0
            };
            if drained {
                return Ok(());
            }
            notified.await;
        }
    }
    pub(super) async fn join_writers(&self) -> Result<()> {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let drained = {
                let state = self.lock()?;
                if state.active == 0 {
                    ensure!(!state.failed, "physical context resource writer panicked");
                    true
                } else {
                    false
                }
            };
            if drained {
                return Ok(());
            }
            notified.await;
        }
    }
}
impl super::RuntimeHost {
    pub(crate) fn context_resources(&self) -> Arc<ContextResources> {
        self.inner.context_resources.clone()
    }
}
impl super::RuntimeHostBridge {
    pub(crate) fn context_resources(&self) -> Result<Arc<ContextResources>> {
        Ok(self.host()?.context_resources())
    }
}
