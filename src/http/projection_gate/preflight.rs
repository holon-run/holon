use tokio::sync::oneshot;
use tracing::Callsite as _;

use super::*;

// Separate from configured build leaders: total supervised capacity is B + 1.
pub(super) const MAX_WORKERS: usize = 1;

struct CallerGuard(RuntimeDbReadScope);

impl Drop for CallerGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct WorkerGuard {
    gate: Arc<ProjectionGate>,
    id: u64,
    _permit: OwnedSemaphorePermit,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.gate
            .state
            .lock()
            .expect("projection gate lock poisoned")
            .preflights
            .remove(&self.id);
    }
}

fn cancelled(deadline: Instant) -> ProjectionFailure {
    if deadline <= Instant::now() {
        ProjectionFailure::deadline_exhausted()
    } else {
        ProjectionFailure::leader_cancelled()
    }
}

fn saturated() -> ProjectionFailure {
    ProjectionFailure {
        status: StatusCode::SERVICE_UNAVAILABLE,
        body: json!({
            "ok": false,
            "code": "projection_preflight_saturated",
            "error": "projection capability preflight capacity exhausted",
            "retryable": true,
        }),
    }
}

fn emit_warning(code: Option<&str>, status: StatusCode, message: &'static str) {
    static __CALLSITE: tracing::callsite::DefaultCallsite = tracing::callsite2! {
        name: "projection_preflight_failure",
        kind: tracing::metadata::Kind::EVENT,
        target: module_path!(),
        level: tracing::Level::WARN,
        fields: code, status, message
    };
    let metadata = __CALLSITE.metadata();
    // Thread-local dispatchers can change after a callsite caches `never`.
    // Check the actual dispatcher without consulting that shared interest cache.
    let enabled = tracing::dispatcher::get_default(|dispatcher| dispatcher.enabled(metadata));
    if enabled {
        let fields = metadata.fields();
        tracing::Event::dispatch(
            metadata,
            &tracing::valueset!(fields, code = code, status = %status, message = message),
        );
    }
}

impl ProjectionGate {
    /// Every caller must verify capabilities, including cache hits and followers.
    /// This typed lane neither caches results nor selects a stale projection.
    pub(crate) async fn preflight<T, F>(
        self: &Arc<Self>,
        deadline: Instant,
        work: F,
    ) -> Result<T, ProjectionFailure>
    where
        T: Send + 'static,
        F: FnOnce(RuntimeDbReadScope) -> Result<T, ProjectionFailure> + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        let scope = {
            let mut state = self.state.lock().expect("projection gate lock poisoned");
            while let Some(result) = state.tasks.try_join_next() {
                owned::observe_join(result);
            }
            if deadline <= Instant::now() {
                return Err(ProjectionFailure::deadline_exhausted());
            }
            if state.closed {
                return Err(ProjectionFailure::leader_cancelled());
            }
            let permit = Arc::clone(&self.preflight_workers)
                .try_acquire_owned()
                .map_err(|_| {
                    diagnostics::record_projection_gate_rejected();
                    saturated()
                })?;
            state.next_preflight = state
                .next_preflight
                .checked_add(1)
                .expect("projection preflight identity exhausted");
            let id = state.next_preflight;
            let scope = RuntimeDbReadScope::new(deadline.into_std());
            state.preflights.insert(id, scope.clone());
            // Construct before spawning, so queued work also owns registration.
            let worker = WorkerGuard {
                gate: Arc::clone(self),
                id,
                _permit: permit,
            };
            let worker_scope = scope.clone();
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            state.tasks.spawn_blocking(move || {
                let _worker = worker;
                let _dispatch = tracing::dispatcher::set_default(&dispatch);
                let result = if deadline <= Instant::now() || worker_scope.is_cancelled() {
                    Err(cancelled(deadline))
                } else {
                    work(worker_scope)
                };
                // A successful send is not a consumption acknowledgement.
                // Observe non-retryable failures before transferring ownership.
                if let Err(failure) = &result {
                    if !is_retryable_service_unavailable(failure) {
                        emit_warning(
                            failure.error_code(),
                            failure.status(),
                            "projection preflight returned a non-retryable failure",
                        );
                    }
                }
                if let Err(Err(failure)) = sender.send(result) {
                    if is_retryable_service_unavailable(&failure) {
                        emit_warning(
                            failure.error_code(),
                            failure.status(),
                            "closed projection preflight returned a late failure",
                        );
                    }
                }
            });
            scope
        };
        let caller = CallerGuard(scope.clone());
        let cancellation = Some(scope.clone());
        let result = tokio::select! {
            biased;
            result = receiver => result.unwrap_or_else(|_| Err(ProjectionFailure {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: json!({
                    "ok": false,
                    "code": "projection_preflight_worker_failed",
                    "error": "projection preflight worker exited without a result",
                    "retryable": false,
                }),
            })),
            () = RuntimeDbReadScope::cancelled(&cancellation) => Err(cancelled(deadline)),
            () = tokio::time::sleep_until(deadline) => Err(ProjectionFailure::deadline_exhausted()),
        };
        let result = match result {
            // Already acquired protection failures must not become cancellation.
            Err(failure) if !is_retryable_service_unavailable(&failure) => Err(failure),
            _ if deadline <= Instant::now() || scope.is_cancelled() => Err(cancelled(deadline)),
            result => result,
        };
        drop(caller);
        result
    }
}

#[cfg(test)]
mod tests;
