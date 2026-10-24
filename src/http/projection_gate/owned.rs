use std::future::poll_fn;

use tokio::sync::oneshot;

use super::*;

#[derive(Debug)]
pub(super) struct BuildLease {
    _permit: OwnedSemaphorePermit,
}

impl BuildLease {
    pub(super) fn new(permit: OwnedSemaphorePermit) -> Self {
        Self { _permit: permit }
    }
}

/// The scope and capacity belong to the shared build, never its first caller.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionBuildContext {
    gate: Arc<ProjectionGate>,
    lease: Arc<BuildLease>,
    scope: RuntimeDbReadScope,
}

impl ProjectionBuildContext {
    #[cfg(test)]
    pub(crate) fn read_scope(&self) -> RuntimeDbReadScope {
        self.scope.clone()
    }

    /// Register before spawning; both queued and running work own capacity.
    /// Dropping the receiver does not detach the worker from gate shutdown.
    pub(crate) async fn blocking<T, F>(&self, work: F) -> Result<T, ProjectionFailure>
    where
        T: Send + 'static,
        F: FnOnce(RuntimeDbReadScope) -> T + Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self
                .gate
                .state
                .lock()
                .expect("projection gate lock poisoned");
            if state.closed || self.scope.is_cancelled() {
                return Err(ProjectionFailure::leader_cancelled());
            }
            let lease = Arc::clone(&self.lease);
            let scope = self.scope.clone();
            state.tasks.spawn_blocking(move || {
                let _lease = lease;
                let result = work(scope);
                let _ = sender.send(result);
            });
        }
        receiver.await.map_err(|_| ProjectionFailure {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: json!({
                "ok": false,
                "code": "projection_worker_failed",
                "error": "projection worker exited without a result",
                "retryable": false,
            }),
        })
    }
}

impl ProjectionFailure {
    pub(super) fn deadline_exhausted() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: json!({
                "ok": false,
                "code": "projection_deadline_exhausted",
                "error": "projection deadline exhausted",
                "retryable": true,
            }),
        }
    }
}

struct WaiterGuard {
    gate: Arc<ProjectionGate>,
    key: ProjectionKey,
    flight: Arc<Flight>,
    id: u64,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("projection gate lock poisoned");
        if !entry_matches_flight(state.entries.get(&self.key), &self.flight) {
            return;
        }
        if let Some(Entry::InFlight { waiters, .. }) = state.entries.get_mut(&self.key) {
            waiters.remove(&self.id);
            waiters.retain(|_, deadline| *deadline > Instant::now());
            if !waiters.is_empty() {
                return;
            }
        }
        state.entries.remove(&self.key);
        let _ = close_flight(
            &self.flight,
            Err(ProjectionFailure::leader_cancelled().into()),
        );
    }
}

pub(super) fn has_live_waiters(state: &GateState, key: &ProjectionKey, flight: &Flight) -> bool {
    !flight
        .deadline
        .is_some_and(|deadline| deadline <= Instant::now())
        && !flight
            .scope
            .as_ref()
            .is_some_and(RuntimeDbReadScope::is_cancelled)
        && matches!(
            state.entries.get(key),
            Some(Entry::InFlight { waiters, .. })
                if waiters.values().any(|deadline| *deadline > Instant::now())
        )
}

pub(super) fn observe_join(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        tracing::error!(%error, "supervised projection task failed");
    }
}

impl ProjectionGate {
    fn close_cancelled_build(&self, key: &ProjectionKey, flight: &Arc<Flight>) {
        let mut state = self.state.lock().expect("projection gate lock poisoned");
        if entry_matches_flight(state.entries.get(key), flight) {
            state.entries.remove(key);
        }
        let failure = if flight
            .deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            ProjectionFailure::deadline_exhausted()
        } else {
            ProjectionFailure::leader_cancelled()
        };
        let _ = close_flight(flight, Err(failure.into()));
    }

    /// Admission, waiter registration, and task registration share the gate lock.
    pub(crate) async fn run_owned<F, Fut>(
        self: &Arc<Self>,
        key: ProjectionKey,
        caller_deadline: Instant,
        build_budget: Duration,
        build: F,
    ) -> ProjectionDelivery
    where
        F: FnOnce(ProjectionBuildContext) -> Fut + Send + 'static,
        Fut: Future<Output = ProjectionResult> + Send + 'static,
    {
        let waiter = {
            let _decision = diagnostics::attribution::PROJECTION_GATE.start();
            let mut state = self.state.lock().expect("projection gate lock poisoned");
            while let Some(result) = state.tasks.try_join_next() {
                observe_join(result);
            }
            if caller_deadline <= Instant::now() {
                return Err(ProjectionFailure::deadline_exhausted().into());
            }
            if state.closed {
                diagnostics::record_projection_gate_rejected();
                return Err(ProjectionGateError::Rejected);
            }
            if let Some(Entry::Ready { bytes, expires_at }) = state.entries.get(&key) {
                if *expires_at > Instant::now() {
                    diagnostics::record_projection_gate_cache_hit();
                    return Ok(bytes.clone());
                }
            }
            // A delayed timer must not let a new caller join an expired flight.
            if let Some(Entry::InFlight { flight, .. }) = state.entries.get(&key) {
                if flight
                    .deadline
                    .is_some_and(|deadline| deadline <= Instant::now())
                {
                    let _ =
                        close_flight(flight, Err(ProjectionFailure::deadline_exhausted().into()));
                    state.entries.remove(&key);
                }
            }
            diagnostics::record_projection_gate_cache_miss();
            state.next_waiter = state
                .next_waiter
                .checked_add(1)
                .expect("projection waiter identity exhausted");
            let id = state.next_waiter;
            let flight = if let Some(Entry::InFlight { flight, waiters }) =
                state.entries.get_mut(&key)
            {
                waiters.insert(id, caller_deadline);
                diagnostics::record_projection_gate_joined_waiter();
                Arc::clone(flight)
            } else {
                let permit = match Arc::clone(&self.leaders).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        diagnostics::record_projection_gate_rejected();
                        return Err(ProjectionGateError::Rejected);
                    }
                };
                state.entries.retain(|_, entry| {
                    !matches!(entry, Entry::Ready { expires_at, .. } if *expires_at <= Instant::now())
                });
                let deadline = Instant::now() + build_budget;
                let scope = RuntimeDbReadScope::new(deadline.into_std());
                let (sender, _) = watch::channel(FlightState::Pending);
                let flight = Arc::new(Flight {
                    state: sender,
                    scope: Some(scope.clone()),
                    deadline: Some(deadline),
                });
                state.entries.insert(
                    key.clone(),
                    Entry::InFlight {
                        flight: Arc::clone(&flight),
                        waiters: HashMap::from([(id, caller_deadline)]),
                    },
                );
                let context = ProjectionBuildContext {
                    gate: Arc::clone(self),
                    lease: Arc::new(BuildLease::new(permit)),
                    scope,
                };
                let gate = Arc::clone(self);
                let worker_key = key.clone();
                let worker_flight = Arc::clone(&flight);
                // Registration precedes close, even if the task has not polled.
                state.tasks.spawn(async move {
                    let mut guard = LeaderGuard::with_lease(
                        &gate,
                        worker_key.clone(),
                        Arc::clone(&worker_flight),
                        Arc::clone(&context.lease),
                    );
                    let _build = diagnostics::attribution::PROJECTION_BUILD.start();
                    if deadline <= Instant::now() || context.scope.is_cancelled() {
                        gate.close_cancelled_build(&worker_key, &worker_flight);
                        return;
                    }
                    let cancellation = Some(context.scope.clone());
                    let future = build(context);
                    tokio::pin!(future);
                    let result = tokio::select! {
                        biased;
                        result = &mut future => Some(result),
                        () = RuntimeDbReadScope::cancelled(&cancellation) => None,
                        () = tokio::time::sleep_until(deadline) => {
                            gate.close_cancelled_build(&worker_key, &worker_flight);
                            None
                        }
                    };
                    let result = match result {
                        Some(result) => result,
                        None => {
                            // Close before draining, without inventing a stale
                            // response while protection checks are unfinished.
                            gate.close_cancelled_build(&worker_key, &worker_flight);
                            let result = future.await;
                            if let Err(failure) = &result {
                                tracing::warn!(
                                    code = failure.error_code(),
                                    status = %failure.status(),
                                    "closed projection worker returned a late failure"
                                );
                            }
                            result
                        }
                    };
                    if result.is_err() {
                        diagnostics::record_projection_gate_failed();
                    }
                    let _ = guard.finish(result);
                });
                flight
            };
            WaiterGuard {
                gate: Arc::clone(self),
                key,
                flight,
                id,
            }
        };
        let _wait = diagnostics::attribution::PROJECTION_FOLLOWER.start();
        let result =
            tokio::time::timeout_at(caller_deadline, wait_for_flight(Arc::clone(&waiter.flight)))
                .await
                .unwrap_or_else(|_| Err(ProjectionFailure::deadline_exhausted().into()));
        drop(waiter);
        self.record_delivery(result)
    }

    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("projection gate lock poisoned");
        state.closed = true;
        state.stale.clear();
        for scope in state.preflights.values() {
            scope.cancel();
        }
        for (_, entry) in state.entries.drain() {
            if let Entry::InFlight { flight, .. } = entry {
                let _ = close_flight(&flight, Err(ProjectionFailure::leader_cancelled().into()));
            }
        }
    }

    pub(crate) async fn close_and_drain(&self) {
        self.close();
        self.drain_tasks().await;
    }

    pub(super) async fn drain_tasks(&self) {
        // A cancelled drain future leaves the registry in the gate, not in a
        // dropped local JoinSet. A later owner can resume the actual joins.
        let _drain = self.drain.lock().await;
        loop {
            let result = poll_fn(|cx| {
                let mut state = self.state.lock().expect("projection gate lock poisoned");
                state.tasks.poll_join_next(cx)
            })
            .await;
            match result {
                Some(result) => observe_join(result),
                None => return,
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod sql_tests;
