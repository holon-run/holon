use tokio::sync::oneshot;
use tracing::Callsite as _;

use super::*;

// Separate from configured build leaders: total supervised capacity is B + 1.
pub(super) const MAX_WORKERS: usize = 1;
// Bounded async admission queue, independent of MAX_WORKERS and build leaders.
const MAX_WAITERS: usize = 16;

struct CallerGuard(RuntimeDbReadScope);

impl Drop for CallerGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// The worker permit handed from an exiting worker (or a bailing caller) to
/// the oldest queued caller under the gate lock.
#[derive(Debug)]
struct AdmissionHandoff {
    permit: OwnedSemaphorePermit,
}

/// A queued admission waiter. It owns no read scope and runs no SQL; gate
/// close drops the sender, which fails the waiter with the existing
/// cancellation semantics.
#[derive(Debug)]
pub(super) struct PreflightWaiter {
    id: u64,
    admitted: oneshot::Sender<AdmissionHandoff>,
}

/// RAII ownership of one admission queue slot, or of a fast-path permit.
/// Dropping it removes the waiter, or forwards a received-but-unregistered
/// permit to the next waiter so a permit never bypasses older waiters.
struct WaiterGuard {
    gate: Arc<ProjectionGate>,
    // Zero means the caller never queued (fast-path admission).
    id: u64,
    permit: Option<OwnedSemaphorePermit>,
}

impl WaiterGuard {
    fn take_permit(&mut self) -> Option<OwnedSemaphorePermit> {
        self.permit.take()
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("projection gate lock poisoned");
        state
            .preflight_waiters
            .retain(|waiter| waiter.id != self.id);
        if let Some(permit) = self.permit.take() {
            hand_off(&mut state, permit);
        }
    }
}

struct WorkerGuard {
    gate: Arc<ProjectionGate>,
    id: u64,
    _permit: Option<OwnedSemaphorePermit>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        let permit = self._permit.take();
        let mut state = self
            .gate
            .state
            .lock()
            .expect("projection gate lock poisoned");
        state.preflights.remove(&self.id);
        if let Some(permit) = permit {
            hand_off(&mut state, permit);
        }
    }
}

/// Transfers one worker permit to the oldest queued waiter under the gate
/// lock. A permit returns to fresh callers only when no waiter accepts it.
fn hand_off(state: &mut GateState, mut permit: OwnedSemaphorePermit) {
    while let Some(waiter) = state.preflight_waiters.pop_front() {
        match waiter.admitted.send(AdmissionHandoff { permit }) {
            Ok(()) => return,
            Err(AdmissionHandoff { permit: returned }) => {
                // The waiter future is already gone; its guard has run. Offer
                // the same permit to the next queued caller.
                permit = returned;
            }
        }
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
        // The admission guard owns the worker permit from fast-path acquisition
        // or queue handoff until registration. There is no await between
        // acquisition and registration, so a dropped caller can only leave the
        // permit inside the guard, which forwards it to the next waiter.
        let mut admission = self.acquire_preflight_admission(deadline).await?;
        let scope = {
            let mut state = self.state.lock().expect("projection gate lock poisoned");
            while let Some(result) = state.tasks.try_join_next() {
                owned::observe_join(result);
            }
            if deadline <= Instant::now() || state.closed {
                let permit = admission
                    .take_permit()
                    .expect("admission owns the worker permit");
                hand_off(&mut state, permit);
                return Err(if state.closed {
                    ProjectionFailure::leader_cancelled()
                } else {
                    ProjectionFailure::deadline_exhausted()
                });
            }
            spawn_preflight_worker(
                self,
                &mut state,
                admission
                    .take_permit()
                    .expect("admission owns the worker permit"),
                deadline,
                work,
                sender,
            )
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

    /// Acquires the single preflight worker permit, waiting in a bounded FIFO
    /// queue when the worker is busy. Waiting holds no gate lock, registers no
    /// scope, runs no SQL, and counts against the caller's original deadline.
    async fn acquire_preflight_admission(
        self: &Arc<Self>,
        deadline: Instant,
    ) -> Result<WaiterGuard, ProjectionFailure> {
        let (admitted_tx, admitted_rx) = oneshot::channel();
        let mut guard = WaiterGuard {
            gate: Arc::clone(self),
            id: 0,
            permit: None,
        };
        {
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
            match Arc::clone(&self.preflight_workers).try_acquire_owned() {
                Ok(permit) => {
                    guard.permit = Some(permit);
                    return Ok(guard);
                }
                Err(_) if state.preflight_waiters.len() < MAX_WAITERS => {
                    // Waiter identities start at one; zero marks a caller that
                    // never queued.
                    let id = state
                        .next_preflight_waiter
                        .checked_add(1)
                        .expect("projection preflight waiter identity exhausted");
                    state.next_preflight_waiter = id;
                    state.preflight_waiters.push_back(PreflightWaiter {
                        id,
                        admitted: admitted_tx,
                    });
                    guard.id = id;
                }
                Err(_) => {
                    diagnostics::record_projection_gate_rejected();
                    return Err(saturated());
                }
            }
        }
        let permit = {
            let _wait = crate::diagnostics::attribution::PREFLIGHT_ADMISSION_WAIT.start();
            tokio::select! {
                biased;
                admitted = admitted_rx => match admitted {
                    Ok(handoff) => handoff.permit,
                    // The sender only disappears when the gate closes while
                    // this caller is queued.
                    Err(_) => return Err(cancelled(deadline)),
                },
                () = tokio::time::sleep_until(deadline) => {
                    return Err(ProjectionFailure::deadline_exhausted());
                }
            }
        };
        guard.permit = Some(permit);
        Ok(guard)
    }
}

/// Registers one preflight worker under the gate lock and returns its scope.
fn spawn_preflight_worker<T, F>(
    gate: &Arc<ProjectionGate>,
    state: &mut GateState,
    permit: OwnedSemaphorePermit,
    deadline: Instant,
    work: F,
    sender: oneshot::Sender<Result<T, ProjectionFailure>>,
) -> RuntimeDbReadScope
where
    T: Send + 'static,
    F: FnOnce(RuntimeDbReadScope) -> Result<T, ProjectionFailure> + Send + 'static,
{
    state.next_preflight = state
        .next_preflight
        .checked_add(1)
        .expect("projection preflight identity exhausted");
    let id = state.next_preflight;
    let scope = RuntimeDbReadScope::new(deadline.into_std());
    state.preflights.insert(id, scope.clone());
    // Construct before spawning, so queued work also owns registration.
    let worker = WorkerGuard {
        gate: Arc::clone(gate),
        id,
        _permit: Some(permit),
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
}

#[cfg(test)]
mod tests;
