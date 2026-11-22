//! Responsibility belongs to the admitted physical writer, not its waiter.
use super::context_resources::{ContextResources, ResourceWriteScope};
use anyhow::{ensure, Result};
use std::sync::{Arc, OnceLock};

pub struct ResourceWrite {
    resources: Arc<ContextResources>,
    scope: Arc<ResourceWriteScope>,
    physical_scope: OnceLock<Arc<ResourceWriteScope>>,
}
impl ResourceWrite {
    /// Called inside the original blocking kernel, never in its waiter.
    pub fn enter_physical(&self) -> Result<()> {
        if self.physical_scope.get().is_none() {
            let resolved = self.resources.resolve_scope(&self.scope)?;
            let mut state = self.resources.lock()?;
            if self.physical_scope.get().is_none() {
                let scope = if resolved == *self.scope {
                    self.scope.clone()
                } else {
                    ContextResources::changed(&mut state, &resolved)?;
                    let scope = Arc::new(resolved);
                    for active in &mut state.active {
                        if Arc::ptr_eq(active, &self.scope) {
                            *active = scope.clone();
                        }
                    }
                    scope
                };
                let _ = self.physical_scope.set(scope);
            }
        }
        #[cfg(any(test, feature = "http-test-support"))]
        if let Some(barrier) = self.resources.physical_checkpoint.lock().unwrap().take() {
            barrier.reached.notify_one();
            let mut released = barrier.released.lock().unwrap();
            while !*released {
                released = barrier.release.wait(released).unwrap();
            }
        }
        Ok(())
    }
}
#[cfg(any(test, feature = "http-test-support"))]
#[derive(Default)]
pub(crate) struct PhysicalWriteBarrier {
    pub(crate) reached: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    release: std::sync::Condvar,
}
#[cfg(any(test, feature = "http-test-support"))]
pub struct PausedPhysicalWriter(Arc<PhysicalWriteBarrier>);
#[cfg(any(test, feature = "http-test-support"))]
impl PausedPhysicalWriter {
    pub async fn wait(&self) {
        self.0.reached.notified().await;
    }
    pub fn release(&self) {
        *self.0.released.lock().unwrap() = true;
        self.0.release.notify_all();
    }
}
#[cfg(any(test, feature = "http-test-support"))]
impl Drop for PausedPhysicalWriter {
    fn drop(&mut self) {
        self.release();
    }
}
#[cfg(any(test, feature = "http-test-support"))]
impl ContextResources {
    pub fn pause_next_physical_writer(&self) -> PausedPhysicalWriter {
        let barrier = Arc::new(PhysicalWriteBarrier::default());
        *self.physical_checkpoint.lock().unwrap() = Some(barrier.clone());
        PausedPhysicalWriter(barrier)
    }
}
impl ContextResources {
    pub fn begin_path_write(
        self: &Arc<Self>,
        paths: Vec<std::path::PathBuf>,
    ) -> Result<ResourceWrite> {
        self.begin_scoped_write(self.path_scope(paths))
    }
    /// Call only after the original authorization and operation admission.
    pub fn begin_write(self: &Arc<Self>) -> Result<ResourceWrite> {
        self.begin_scoped_write(ResourceWriteScope::shared())
    }
    pub(crate) fn begin_scoped_write(
        self: &Arc<Self>,
        scope: ResourceWriteScope,
    ) -> Result<ResourceWrite> {
        let mut state = self.lock()?;
        ensure!(
            !state.closing,
            "context resource writer admission is Closing"
        );
        let scope = Arc::new(scope);
        Self::changed(&mut state, &scope)?;
        state.active.push(scope.clone());
        Ok(ResourceWrite {
            resources: self.clone(),
            scope,
            physical_scope: OnceLock::new(),
        })
    }
}
impl Drop for ResourceWrite {
    fn drop(&mut self) {
        if let Ok(mut state) = self.resources.lock() {
            let scope = self.physical_scope.get().unwrap_or(&self.scope);
            state.active.retain(|active| !Arc::ptr_eq(active, scope));
            state.failed |= std::thread::panicking();
            // Failure, panic and cancellation are dirty too.
            if let Err(error) = ContextResources::changed(&mut state, scope) {
                tracing::warn!(%error, "resource writer exit invalidation failed");
            }
            self.resources.drained.notify_waiters();
        }
    }
}
#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelling_waiter_keeps_physical_writer_and_closing_join() -> Result<()> {
        let resources = Arc::new(ContextResources::default());
        let writer = resources.begin_write()?;
        let (send, receive) = std::sync::mpsc::channel();
        let kernel = tokio::task::spawn_blocking(move || {
            let _writer = writer;
            receive.recv().unwrap();
        });
        let waiter = tokio::spawn(async move { kernel.await });
        waiter.abort();
        resources.begin_close()?;
        assert!(resources.begin_write().is_err());
        assert!(resources.ready().is_err());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(20),
            resources.join_writers()
        )
        .await
        .is_err());
        send.send(())?;
        resources.join_writers().await?;
        Ok(())
    }
}

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod join_failure_tests {
    use super::*;
    #[tokio::test]
    async fn physical_kernel_panic_keeps_closing_and_returns_join_failure() -> Result<()> {
        let gate = Arc::new(ContextResources::default());
        let writer = gate.begin_write()?;
        let kernel = tokio::task::spawn_blocking(move || {
            let _writer = writer;
            panic!("controlled original physical kernel failure");
        });
        assert!(kernel.await.is_err());
        gate.begin_close()?;
        assert!(gate.join_writers().await.is_err());
        assert!(gate.begin_write().is_err());
        assert!(gate.ready().is_err());
        Ok(())
    }
}
