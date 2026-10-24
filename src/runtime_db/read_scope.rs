use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use super::RuntimeDbRetryableError;

/// Opt-in cancellation and an absolute deadline for pooled read-role work only.
/// Clones share cancellation; cancelling a single read never cancels this scope.
#[derive(Clone, Debug)]
pub struct RuntimeDbReadScope {
    deadline: Instant,
    cancellation: CancellationToken,
}

impl RuntimeDbReadScope {
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled() || Instant::now() >= self.deadline
    }

    pub(crate) fn child(&self) -> Self {
        Self {
            deadline: self.deadline,
            cancellation: self.cancellation.child_token(),
        }
    }

    pub(crate) fn check(&self, path: &Path) -> Result<()> {
        if self.is_cancelled() {
            return Err(RuntimeDbRetryableError::new(
                "scoped reader",
                path,
                "read cancelled or absolute deadline exhausted",
            )
            .into());
        }
        Ok(())
    }

    pub(crate) async fn cancelled(scope: &Option<Self>) {
        match scope {
            Some(scope) => scope.cancellation.cancelled().await,
            None => std::future::pending().await,
        }
    }
}
