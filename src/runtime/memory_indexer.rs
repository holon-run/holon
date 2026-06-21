use std::{sync::atomic::Ordering, time::Duration};

use tracing::{debug, warn};

use super::RuntimeHandle;

const MEMORY_INDEXER_BATCH_LIMIT: usize = 500;
const MEMORY_INDEXER_IDLE_RECHECK: Duration = Duration::from_secs(5);
const MEMORY_INDEXER_ERROR_RECHECK: Duration = Duration::from_secs(10);

impl RuntimeHandle {
    pub(super) fn spawn_background_memory_indexer(&self) {
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.run_background_memory_indexer().await;
        });
    }

    async fn run_background_memory_indexer(self) {
        loop {
            if self.inner.shutdown_requested.load(Ordering::SeqCst) {
                return;
            }

            let active_workspace_id = match self.agent_state().await {
                Ok(state) => state.active_workspace_entry.map(|entry| entry.workspace_id),
                Err(error) => {
                    warn!(error = %error, "memory indexer could not read agent state");
                    self.wait_before_next_memory_indexer_round(MEMORY_INDEXER_ERROR_RECHECK)
                        .await;
                    continue;
                }
            };
            let storage = self.inner.storage.clone();
            let refresh = tokio::task::spawn_blocking(move || {
                crate::memory::refresh_memory_index_bounded(
                    &storage,
                    active_workspace_id.as_deref(),
                    MEMORY_INDEXER_BATCH_LIMIT,
                )
            })
            .await;

            match refresh {
                Ok(Ok(status)) => {
                    debug!(
                        freshness = %status.freshness,
                        lag = status.lag,
                        consumption_was_limited = status.consumption_was_limited,
                        skipped_error_count = status.skipped_error_count,
                        "memory indexer refreshed projection"
                    );
                    if status.lag > 0 || status.consumption_was_limited {
                        tokio::task::yield_now().await;
                    } else {
                        self.wait_before_next_memory_indexer_round(MEMORY_INDEXER_IDLE_RECHECK)
                            .await;
                    }
                }
                Ok(Err(error)) => {
                    warn!(error = %error, "memory indexer refresh failed");
                    self.wait_before_next_memory_indexer_round(MEMORY_INDEXER_ERROR_RECHECK)
                        .await;
                }
                Err(error) => {
                    warn!(error = %error, "memory indexer refresh task failed");
                    self.wait_before_next_memory_indexer_round(MEMORY_INDEXER_ERROR_RECHECK)
                        .await;
                }
            }
        }
    }

    async fn wait_before_next_memory_indexer_round(&self, duration: Duration) {
        tokio::select! {
            _ = self.inner.notify.notified() => {}
            _ = tokio::time::sleep(duration) => {}
        }
    }
}
