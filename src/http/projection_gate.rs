use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};

use axum::{body::Bytes, http::StatusCode, Json};
use serde_json::{json, Value};
use tokio::{
    sync::{watch, OwnedSemaphorePermit, Semaphore},
    time::{Duration, Instant},
};

use crate::diagnostics;
use crate::runtime_db::RuntimeDbReadScope;

mod owned;
mod preflight;
use owned::BuildLease;

const DEFAULT_MAX_LEADERS: usize = crate::config::DEFAULT_API_PROJECTION_MAX_LEADERS as usize;
const DEFAULT_TTL: Duration =
    Duration::from_millis(crate::config::DEFAULT_API_PROJECTION_CACHE_TTL_MS);
pub(crate) const DEFAULT_BUILD_BUDGET: Duration = Duration::from_secs(10);
/// How long the last successful build for a projection key may be served as
/// a stale fallback after a retryable (service-unavailable) assembly
/// failure. Projection assembly budgets are ten seconds; one fallback window
/// covers client retries through a saturation burst without serving
/// arbitrarily old projections.
pub(crate) const PROJECTION_STALE_FALLBACK_MAX_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ProjectionKey {
    AgentsList(Option<String>),
    AgentsRosterSnapshot(String),
    AgentProjectionSnapshot(String, String),
    AgentState(String),
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionFailure {
    status: StatusCode,
    body: Value,
}

impl ProjectionFailure {
    fn leader_cancelled() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: json!({
                "ok": false,
                "error": "projection leader was cancelled",
                "code": "projection_leader_cancelled",
                "retryable": true,
            }),
        }
    }

    pub(crate) fn into_http_error(self) -> (StatusCode, Json<Value>) {
        (self.status, Json(self.body))
    }

    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    pub(crate) fn error_code(&self) -> Option<&str> {
        self.body.get("code").and_then(Value::as_str)
    }

    pub(crate) fn retryable(&self) -> Option<bool> {
        self.body.get("retryable").and_then(Value::as_bool)
    }
}

impl From<(StatusCode, Json<Value>)> for ProjectionFailure {
    fn from((status, Json(body)): (StatusCode, Json<Value>)) -> Self {
        Self { status, body }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ProjectionGateError {
    Build(ProjectionFailure),
    /// The assembly failed retryably and a fresh-enough last good projection
    /// exists. Carries the stale bytes so the caller serves them with the
    /// stale marker instead of a bare success the client would read as fresh.
    StaleServed(Bytes),
    Rejected,
}

type ProjectionResult = Result<Bytes, ProjectionFailure>;
type ProjectionDelivery = Result<Bytes, ProjectionGateError>;

/// The stale fallback must not fire for every HTTP 503: this crate
/// deliberately maps the non-retryable `runtime_db_quarantined` error onto
/// HTTP 503 with `retryable: false` in its envelope. Only assembly failures
/// whose envelope opts into retry (`retryable: true`, e.g. the snapshot
/// budget timeout or a cancelled leader) may serve the last good projection.
fn is_retryable_service_unavailable(failure: &ProjectionFailure) -> bool {
    failure.status == StatusCode::SERVICE_UNAVAILABLE
        && failure.body.get("retryable") == Some(&Value::Bool(true))
}

#[derive(Debug, Clone)]
enum FlightState {
    Pending,
    Finished(ProjectionDelivery),
}

#[derive(Debug)]
struct Flight {
    state: watch::Sender<FlightState>,
    scope: Option<RuntimeDbReadScope>,
    deadline: Option<Instant>,
}

#[derive(Debug)]
enum Entry {
    InFlight {
        flight: Arc<Flight>,
        waiters: HashMap<u64, Instant>,
    },
    Ready {
        bytes: Bytes,
        expires_at: Instant,
    },
}

#[derive(Debug)]
struct StaleEntry {
    bytes: Bytes,
    built_at: Instant,
}

#[derive(Debug, Default)]
struct GateState {
    entries: HashMap<ProjectionKey, Entry>,
    stale: HashMap<ProjectionKey, StaleEntry>,
    tasks: tokio::task::JoinSet<()>,
    preflights: HashMap<u64, RuntimeDbReadScope>,
    next_waiter: u64,
    next_preflight: u64,
    closed: bool,
}

#[derive(Debug)]
pub(crate) struct ProjectionGate {
    state: Mutex<GateState>,
    leaders: Arc<Semaphore>,
    preflight_workers: Arc<Semaphore>,
    ttl: Duration,
    stale_max_age: Duration,
    drain: tokio::sync::Mutex<()>,
    #[cfg(test)]
    stale_deliveries: std::sync::atomic::AtomicU64,
}

impl Default for ProjectionGate {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LEADERS, DEFAULT_TTL)
    }
}

impl ProjectionGate {
    pub(super) fn new(max_leaders: usize, ttl: Duration) -> Self {
        Self::new_with_stale_max_age(max_leaders, ttl, PROJECTION_STALE_FALLBACK_MAX_AGE)
    }

    fn new_with_stale_max_age(max_leaders: usize, ttl: Duration, stale_max_age: Duration) -> Self {
        Self {
            state: Mutex::new(GateState::default()),
            leaders: Arc::new(Semaphore::new(max_leaders)),
            preflight_workers: Arc::new(Semaphore::new(preflight::MAX_WORKERS)),
            ttl,
            stale_max_age,
            drain: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            stale_deliveries: std::sync::atomic::AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) async fn run<F, Fut>(
        &self,
        key: ProjectionKey,
        build: F,
    ) -> Result<Bytes, ProjectionGateError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ProjectionResult>,
    {
        enum Decision<'a> {
            Ready(Bytes),
            Wait(Arc<Flight>),
            Lead(LeaderGuard<'a>),
            Reject,
        }

        let decision = {
            let _decision = diagnostics::attribution::PROJECTION_GATE.start();
            let mut state = self.state.lock().expect("projection gate lock poisoned");
            let now = Instant::now();
            match state.entries.get(&key) {
                _ if state.closed => {
                    diagnostics::record_projection_gate_rejected();
                    Decision::Reject
                }
                Some(Entry::Ready { bytes, expires_at }) if *expires_at > now => {
                    diagnostics::record_projection_gate_cache_hit();
                    Decision::Ready(bytes.clone())
                }
                Some(Entry::InFlight { flight, .. }) => {
                    diagnostics::record_projection_gate_cache_miss();
                    diagnostics::record_projection_gate_joined_waiter();
                    Decision::Wait(Arc::clone(flight))
                }
                _ => {
                    state.entries.retain(|_, entry| {
                        !matches!(
                            entry,
                            Entry::Ready { expires_at, .. } if *expires_at <= now
                        )
                    });
                    diagnostics::record_projection_gate_cache_miss();
                    match Arc::clone(&self.leaders).try_acquire_owned() {
                        Ok(permit) => {
                            let (flight_state, _) = watch::channel(FlightState::Pending);
                            let flight = Arc::new(Flight {
                                state: flight_state,
                                scope: None,
                                deadline: None,
                            });
                            state.entries.insert(
                                key.clone(),
                                Entry::InFlight {
                                    flight: Arc::clone(&flight),
                                    waiters: HashMap::new(),
                                },
                            );
                            Decision::Lead(LeaderGuard::new(self, key, flight, permit))
                        }
                        Err(_) => {
                            diagnostics::record_projection_gate_rejected();
                            Decision::Reject
                        }
                    }
                }
            }
        };

        match decision {
            Decision::Ready(bytes) => Ok(bytes),
            Decision::Wait(flight) => {
                let _wait = diagnostics::attribution::PROJECTION_FOLLOWER.start();
                self.record_delivery(wait_for_flight(flight).await)
            }
            Decision::Lead(mut guard) => {
                let _build = diagnostics::attribution::PROJECTION_BUILD.start();
                let result = build().await;
                if result.is_err() {
                    diagnostics::record_projection_gate_failed();
                }
                self.record_delivery(guard.finish(result))
            }
            Decision::Reject => Err(ProjectionGateError::Rejected),
        }
    }

    /// Model controls must not be overwritten by a cached pre-mutation snapshot.
    pub(crate) fn invalidate_agent(&self, agent_id: &str) {
        let affected = |key: &ProjectionKey| match key {
            ProjectionKey::AgentState(id) | ProjectionKey::AgentProjectionSnapshot(id, _) => {
                id == agent_id
            }
            ProjectionKey::AgentsList(_) | ProjectionKey::AgentsRosterSnapshot(_) => true,
        };
        let mut state = self.state.lock().expect("projection gate lock poisoned");
        state.stale.retain(|key, _| !affected(key));
        state.entries.retain(|key, entry| {
            if !affected(key) {
                return true;
            }
            if let Entry::InFlight { flight, .. } = entry {
                let _ = close_flight(flight, Err(ProjectionFailure::leader_cancelled().into()));
            }
            false
        });
    }
}

impl GateState {
    fn fresh_stale_bytes(&mut self, key: &ProjectionKey, max_age: Duration) -> Option<Bytes> {
        match self.stale.get(key) {
            Some(entry) if entry.built_at.elapsed() <= max_age => Some(entry.bytes.clone()),
            Some(_) => {
                // Expired fallback entries have no other cleanup path; drop
                // them on access so long-lived processes do not accumulate
                // per-key stale bytes without bound.
                self.stale.remove(key);
                diagnostics::record_projection_gate_stale_expired();
                None
            }
            None => None,
        }
    }
}

struct LeaderGuard<'a> {
    gate: &'a ProjectionGate,
    key: ProjectionKey,
    flight: Arc<Flight>,
    _lease: Arc<BuildLease>,
    finished: bool,
}

impl<'a> LeaderGuard<'a> {
    #[cfg(test)]
    fn new(
        gate: &'a ProjectionGate,
        key: ProjectionKey,
        flight: Arc<Flight>,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self::with_lease(gate, key, flight, Arc::new(BuildLease::new(permit)))
    }

    fn with_lease(
        gate: &'a ProjectionGate,
        key: ProjectionKey,
        flight: Arc<Flight>,
        lease: Arc<BuildLease>,
    ) -> Self {
        diagnostics::record_projection_gate_leader_started();
        Self {
            gate,
            key,
            flight,
            _lease: lease,
            finished: false,
        }
    }

    fn finish(&mut self, result: ProjectionResult) -> ProjectionDelivery {
        let mut state = self
            .gate
            .state
            .lock()
            .expect("projection gate lock poisoned");
        if self.flight.scope.is_some()
            && entry_matches_flight(state.entries.get(&self.key), &self.flight)
            && !owned::has_live_waiters(&state, &self.key, &self.flight)
        {
            state.entries.remove(&self.key);
            let failure = match &result {
                Err(failure) if !is_retryable_service_unavailable(failure) => failure.clone(),
                _ if self
                    .flight
                    .deadline
                    .is_some_and(|deadline| deadline <= Instant::now()) =>
                {
                    ProjectionFailure::deadline_exhausted()
                }
                _ => ProjectionFailure::leader_cancelled(),
            };
            let _ = close_flight(&self.flight, Err(failure.into()));
        }
        let delivery = if entry_matches_flight(state.entries.get(&self.key), &self.flight) {
            match result {
                Ok(bytes) => {
                    // Sweep expired entries on insert. Cleanup on access
                    // alone would let never-revisited keys (request-derived
                    // per-agent projections) accumulate stale bytes without
                    // bound in long-lived processes.
                    let built_at = Instant::now();
                    let max_age = self.gate.stale_max_age;
                    let before = state.stale.len();
                    state
                        .stale
                        .retain(|_, entry| entry.built_at.elapsed() <= max_age);
                    diagnostics::record_projection_gate_stale_expired_by(
                        (before - state.stale.len()) as u64,
                    );
                    state.stale.insert(
                        self.key.clone(),
                        StaleEntry {
                            bytes: bytes.clone(),
                            built_at,
                        },
                    );
                    state.entries.insert(
                        self.key.clone(),
                        Entry::Ready {
                            bytes: bytes.clone(),
                            expires_at: Instant::now() + self.gate.ttl,
                        },
                    );
                    Ok(bytes)
                }
                Err(failure) => {
                    state.entries.remove(&self.key);
                    // Bind fallback to this terminal decision, never to a
                    // later cache generation observed by an awakened waiter.
                    match is_retryable_service_unavailable(&failure)
                        .then(|| state.fresh_stale_bytes(&self.key, self.gate.stale_max_age))
                        .flatten()
                    {
                        Some(bytes) => Err(ProjectionGateError::StaleServed(bytes)),
                        None => Err(ProjectionGateError::Build(failure)),
                    }
                }
            }
        } else {
            Err(ProjectionFailure::leader_cancelled().into())
        };
        let delivery = close_flight(&self.flight, delivery);
        self.finished = true;
        delivery
    }
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let mut state = self
                .gate
                .state
                .lock()
                .expect("projection gate lock poisoned");
            let delivery = if entry_matches_flight(state.entries.get(&self.key), &self.flight) {
                state.entries.remove(&self.key);
                match self
                    .flight
                    .scope
                    .is_none()
                    .then(|| state.fresh_stale_bytes(&self.key, self.gate.stale_max_age))
                    .flatten()
                {
                    Some(bytes) => Err(ProjectionGateError::StaleServed(bytes)),
                    None => Err(ProjectionFailure::leader_cancelled().into()),
                }
            } else {
                Err(ProjectionFailure::leader_cancelled().into())
            };
            let _ = close_flight(&self.flight, delivery);
            diagnostics::record_projection_gate_cancelled();
        }
        diagnostics::record_projection_gate_leader_finished();
    }
}

fn entry_matches_flight(entry: Option<&Entry>, flight: &Arc<Flight>) -> bool {
    matches!(
        entry,
        Some(Entry::InFlight { flight: current, .. }) if Arc::ptr_eq(current, flight)
    )
}

impl From<ProjectionFailure> for ProjectionGateError {
    fn from(failure: ProjectionFailure) -> Self {
        Self::Build(failure)
    }
}

/// Called only under the gate state lock. Terminal delivery is immutable even
/// when an invalidated worker finishes after a replacement flight has started.
fn close_flight(flight: &Flight, delivery: ProjectionDelivery) -> ProjectionDelivery {
    if let FlightState::Finished(committed) = &*flight.state.borrow() {
        return committed.clone();
    }
    flight
        .state
        .send_replace(FlightState::Finished(delivery.clone()));
    if let Some(scope) = &flight.scope {
        scope.cancel();
    }
    delivery
}

impl ProjectionGate {
    fn record_delivery(&self, delivery: ProjectionDelivery) -> ProjectionDelivery {
        if matches!(&delivery, Err(ProjectionGateError::StaleServed(_))) {
            tracing::warn!(
                "projection assembly failed retryably; serving the last good projection marked stale"
            );
            diagnostics::record_projection_gate_stale_served();
            // Keep per-gate assertions independent of concurrent requests.
            #[cfg(test)]
            self.stale_deliveries
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        delivery
    }
}

async fn wait_for_flight(flight: Arc<Flight>) -> ProjectionDelivery {
    let mut state = flight.state.subscribe();
    loop {
        if let FlightState::Finished(result) = state.borrow().clone() {
            return result;
        }
        if state.changed().await.is_err() {
            return Err(ProjectionFailure::leader_cancelled().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;

    fn roster_key() -> ProjectionKey {
        ProjectionKey::AgentsRosterSnapshot("principal".into())
    }

    // Hold the same guard used by run so tests can fix either lock order
    // without scheduler sleeps or a probabilistic concurrent race.
    fn pending_leader(gate: &ProjectionGate, key: ProjectionKey) -> LeaderGuard<'_> {
        let permit = Arc::clone(&gate.leaders).try_acquire_owned().unwrap();
        let (state, _) = watch::channel(FlightState::Pending);
        let flight = Arc::new(Flight {
            state,
            scope: None,
            deadline: None,
        });
        gate.state.lock().unwrap().entries.insert(
            key.clone(),
            Entry::InFlight {
                flight: Arc::clone(&flight),
                waiters: HashMap::new(),
            },
        );
        LeaderGuard::new(gate, key, flight, permit)
    }

    #[tokio::test]
    async fn invalidate_before_finish_prevents_fresh_and_stale_publication() {
        tokio::time::pause();
        for result in [
            Ok(Bytes::from_static(b"invalidated")),
            Err(service_unavailable_failure()),
        ] {
            let gate = ProjectionGate::new(1, Duration::from_millis(250));
            gate.run(roster_key(), || async { Ok(Bytes::from_static(b"old")) })
                .await
                .unwrap();
            tokio::time::advance(Duration::from_millis(251)).await;
            let mut leader = pending_leader(&gate, roster_key());
            let flight = Arc::clone(&leader.flight);

            gate.invalidate_agent("agent");
            assert!(matches!(
                leader.finish(result),
                Err(ProjectionGateError::Build(_))
            ));
            assert!(matches!(
                wait_for_flight(flight).await,
                Err(ProjectionGateError::Build(_))
            ));
            let state = gate.state.lock().unwrap();
            assert!(state.entries.is_empty());
            assert!(state.stale.is_empty());
        }
    }

    #[tokio::test]
    async fn failed_flight_delivery_never_reads_a_replacement_fallback() {
        tokio::time::pause();
        let gate = ProjectionGate::new(2, Duration::from_millis(250));
        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"old")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;
        let mut leader = pending_leader(&gate, roster_key());
        let flight = Arc::clone(&leader.flight);
        assert!(matches!(
            leader.finish(Err(service_unavailable_failure())),
            Err(ProjectionGateError::StaleServed(_))
        ));

        gate.invalidate_agent("agent");
        gate.run(roster_key(), || async {
            Ok(Bytes::from_static(b"replacement"))
        })
        .await
        .unwrap();
        match wait_for_flight(flight).await {
            Err(ProjectionGateError::StaleServed(bytes)) => {
                assert_eq!(bytes, Bytes::from_static(b"old"));
            }
            other => panic!("expected the committed old fallback, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finish_before_invalidate_keeps_delivery_but_removes_cache() {
        let gate = ProjectionGate::new(1, Duration::from_millis(250));
        let mut leader = pending_leader(&gate, roster_key());
        let flight = Arc::clone(&leader.flight);
        assert_eq!(
            leader.finish(Ok(Bytes::from_static(b"committed"))).unwrap(),
            Bytes::from_static(b"committed")
        );
        gate.invalidate_agent("agent");
        assert_eq!(
            wait_for_flight(flight).await.unwrap(),
            Bytes::from_static(b"committed")
        );
        let state = gate.state.lock().unwrap();
        assert!(state.entries.is_empty());
        assert!(state.stale.is_empty());
    }

    #[tokio::test]
    async fn invalidated_guard_drop_cannot_remove_or_close_replacement() {
        let gate = ProjectionGate::new(2, Duration::from_millis(250));
        let old_leader = pending_leader(&gate, roster_key());
        let old_flight = Arc::clone(&old_leader.flight);
        gate.invalidate_agent("agent");
        let mut replacement = pending_leader(&gate, roster_key());
        let new_flight = Arc::clone(&replacement.flight);
        drop(old_leader);

        assert!(entry_matches_flight(
            gate.state.lock().unwrap().entries.get(&roster_key()),
            &new_flight
        ));
        assert!(matches!(
            wait_for_flight(old_flight).await,
            Err(ProjectionGateError::Build(_))
        ));
        replacement
            .finish(Ok(Bytes::from_static(b"replacement")))
            .unwrap();
        assert_eq!(
            wait_for_flight(new_flight).await.unwrap(),
            Bytes::from_static(b"replacement")
        );
    }

    #[tokio::test]
    async fn concurrent_requests_for_the_same_key_share_one_build() {
        let gate = Arc::new(ProjectionGate::new(4, Duration::from_millis(250)));
        let builds = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut requests = Vec::new();

        for _ in 0..50 {
            let gate = Arc::clone(&gate);
            let builds = Arc::clone(&builds);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            requests.push(tokio::spawn(async move {
                gate.run(ProjectionKey::AgentsList(None), || async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    release.notified().await;
                    Ok(Bytes::from_static(b"shared"))
                })
                .await
            }));
        }

        started.notified().await;
        tokio::task::yield_now().await;
        release.notify_one();
        for request in requests {
            assert_eq!(
                request.await.unwrap().unwrap(),
                Bytes::from_static(b"shared")
            );
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cached_bytes_expire_after_the_ttl() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));
        let builds = AtomicUsize::new(0);

        let first = gate
            .run(ProjectionKey::AgentsList(None), || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from_static(b"first"))
            })
            .await
            .unwrap();
        let cached = gate
            .run(ProjectionKey::AgentsList(None), || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from_static(b"unexpected"))
            })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;
        let rebuilt = gate
            .run(ProjectionKey::AgentsList(None), || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from_static(b"rebuilt"))
            })
            .await
            .unwrap();

        assert_eq!(first, Bytes::from_static(b"first"));
        assert_eq!(cached, first);
        assert_eq!(rebuilt, Bytes::from_static(b"rebuilt"));
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cache_miss_prunes_expired_ready_entries() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));

        gate.run(
            ProjectionKey::AgentState("removed-agent".into()),
            || async { Ok(Bytes::from_static(b"stale")) },
        )
        .await
        .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;
        gate.run(ProjectionKey::AgentsList(None), || async {
            Ok(Bytes::from_static(b"agents"))
        })
        .await
        .unwrap();

        let state = gate.state.lock().unwrap();
        assert!(!state
            .entries
            .contains_key(&ProjectionKey::AgentState("removed-agent".into())));
        assert!(state.entries.contains_key(&ProjectionKey::AgentsList(None)));
    }

    #[tokio::test]
    async fn distinct_projection_keys_do_not_share_results() {
        let gate = ProjectionGate::new(4, Duration::from_millis(250));
        let agents = gate
            .run(ProjectionKey::AgentsList(None), || async {
                Ok(Bytes::from_static(b"agents"))
            })
            .await
            .unwrap();
        let agent_a = gate
            .run(ProjectionKey::AgentState("agent-a".into()), || async {
                Ok(Bytes::from_static(b"agent-a"))
            })
            .await
            .unwrap();
        let agent_b = gate
            .run(ProjectionKey::AgentState("agent-b".into()), || async {
                Ok(Bytes::from_static(b"agent-b"))
            })
            .await
            .unwrap();

        assert_eq!(agents, Bytes::from_static(b"agents"));
        assert_eq!(agent_a, Bytes::from_static(b"agent-a"));
        assert_eq!(agent_b, Bytes::from_static(b"agent-b"));
    }

    #[tokio::test]
    async fn distinct_parent_filters_do_not_share_results() {
        let gate = ProjectionGate::new(4, Duration::from_millis(250));
        let main = gate
            .run(ProjectionKey::AgentsList(Some("main".into())), || async {
                Ok(Bytes::from_static(b"main-agents"))
            })
            .await
            .unwrap();
        let other = gate
            .run(ProjectionKey::AgentsList(Some("other".into())), || async {
                Ok(Bytes::from_static(b"other-agents"))
            })
            .await
            .unwrap();
        let unfiltered = gate
            .run(ProjectionKey::AgentsList(None), || async {
                Ok(Bytes::from_static(b"all-agents"))
            })
            .await
            .unwrap();

        assert_eq!(main, Bytes::from_static(b"main-agents"));
        assert_eq!(other, Bytes::from_static(b"other-agents"));
        assert_eq!(unfiltered, Bytes::from_static(b"all-agents"));
    }

    #[tokio::test]
    async fn saturated_gate_rejects_new_keys_but_allows_existing_waiters() {
        let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());

        let leader = {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                gate.run(ProjectionKey::AgentState("agent-a".into()), || async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(Bytes::from_static(b"agent-a"))
                })
                .await
            })
        };
        started.notified().await;

        let rejected = gate
            .run(ProjectionKey::AgentState("agent-b".into()), || async {
                Ok(Bytes::from_static(b"agent-b"))
            })
            .await;
        assert!(matches!(rejected, Err(ProjectionGateError::Rejected)));

        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(ProjectionKey::AgentState("agent-a".into()), || async {
                    Ok(Bytes::from_static(b"unexpected"))
                })
                .await
            })
        };
        release.notify_one();

        assert_eq!(
            leader.await.unwrap().unwrap(),
            Bytes::from_static(b"agent-a")
        );
        assert_eq!(
            waiter.await.unwrap().unwrap(),
            Bytes::from_static(b"agent-a")
        );
    }

    #[tokio::test]
    async fn cancelled_leader_releases_the_key_and_waiters() {
        let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
        let started = Arc::new(Notify::new());
        let pending = Arc::new(Notify::new());

        let leader = {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            let pending = Arc::clone(&pending);
            tokio::spawn(async move {
                gate.run(ProjectionKey::AgentsList(None), || async move {
                    started.notify_one();
                    pending.notified().await;
                    Ok(Bytes::from_static(b"never"))
                })
                .await
            })
        };
        started.notified().await;

        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(ProjectionKey::AgentsList(None), || async {
                    Ok(Bytes::from_static(b"unexpected"))
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        leader.abort();
        assert!(matches!(
            waiter.await.unwrap(),
            Err(ProjectionGateError::Build(_))
        ));

        let retry = gate
            .run(ProjectionKey::AgentsList(None), || async {
                Ok(Bytes::from_static(b"retry"))
            })
            .await
            .unwrap();
        assert_eq!(retry, Bytes::from_static(b"retry"));
    }

    #[tokio::test]
    async fn cancelled_current_leader_preserves_valid_stale_delivery() {
        tokio::time::pause();
        let gate = ProjectionGate::new(1, Duration::from_millis(250));
        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"first")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let leader = pending_leader(&gate, roster_key());
        let flight = Arc::clone(&leader.flight);
        drop(leader);

        match wait_for_flight(flight).await {
            Err(ProjectionGateError::StaleServed(bytes)) => {
                assert_eq!(bytes, Bytes::from_static(b"first"));
            }
            other => panic!(
                "expected cancelled leader's stale fallback, got {:?}",
                other
            ),
        }
        assert_eq!(
            gate.run(roster_key(), || async { Ok(Bytes::from_static(b"retry")) })
                .await
                .unwrap(),
            Bytes::from_static(b"retry")
        );
    }

    #[tokio::test]
    async fn failed_leader_releases_the_key_for_retry() {
        let gate = Arc::new(ProjectionGate::new(1, Duration::from_millis(250)));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());

        let leader = {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                gate.run(ProjectionKey::AgentsList(None), || async move {
                    started.notify_one();
                    release.notified().await;
                    Err(ProjectionFailure {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        body: json!({ "error": "failed" }),
                    })
                })
                .await
            })
        };
        started.notified().await;

        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(ProjectionKey::AgentsList(None), || async {
                    Ok(Bytes::from_static(b"unexpected"))
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        release.notify_one();

        assert!(matches!(
            leader.await.unwrap(),
            Err(ProjectionGateError::Build(_))
        ));
        assert!(matches!(
            waiter.await.unwrap(),
            Err(ProjectionGateError::Build(_))
        ));

        let retry = gate
            .run(ProjectionKey::AgentsList(None), || async {
                Ok(Bytes::from_static(b"retry"))
            })
            .await
            .unwrap();
        assert_eq!(retry, Bytes::from_static(b"retry"));
    }

    fn service_unavailable_failure() -> ProjectionFailure {
        ProjectionFailure {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: json!({
                "error": "roster snapshot assembly exceeded the budget",
                "retryable": true,
            }),
        }
    }

    #[tokio::test]
    async fn observer_stale_fallback_never_crosses_principals() {
        tokio::time::pause();
        for (key_a, key_b) in [
            (
                ProjectionKey::AgentsRosterSnapshot("user-a".into()),
                ProjectionKey::AgentsRosterSnapshot("user-b".into()),
            ),
            (
                ProjectionKey::AgentProjectionSnapshot("web".into(), "user-a".into()),
                ProjectionKey::AgentProjectionSnapshot("web".into(), "user-b".into()),
            ),
        ] {
            let gate = ProjectionGate::new(4, Duration::from_millis(250));
            gate.run(key_a.clone(), || async {
                Ok(Bytes::from_static(b"user-a"))
            })
            .await
            .unwrap();
            tokio::time::advance(Duration::from_millis(251)).await;
            assert!(matches!(
                gate.run(key_b, || async { Err(service_unavailable_failure()) })
                    .await,
                Err(ProjectionGateError::Build(_))
            ));
            assert!(matches!(
                gate.run(key_a, || async { Err(service_unavailable_failure()) })
                    .await,
                Err(ProjectionGateError::StaleServed(bytes)) if bytes == Bytes::from_static(b"user-a")
            ));
        }
    }

    #[tokio::test]
    async fn retryable_failure_serves_last_good_bytes_and_releases_the_key() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));
        let builds = AtomicUsize::new(0);

        let first = gate
            .run(roster_key(), || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from_static(b"first"))
            })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let stale = gate
            .run(roster_key(), || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Err(service_unavailable_failure())
            })
            .await;
        match stale {
            Err(ProjectionGateError::StaleServed(bytes)) => {
                assert_eq!(bytes, Bytes::from_static(b"first"))
            }
            other => panic!("expected stale fallback, got {:?}", other),
        }
        assert_eq!(first, Bytes::from_static(b"first"));

        let rebuilt = gate
            .run(roster_key(), || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from_static(b"rebuilt"))
            })
            .await
            .unwrap();
        assert_eq!(rebuilt, Bytes::from_static(b"rebuilt"));
        assert_eq!(builds.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_service_unavailable_failure_still_surfaces() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));

        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"first")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let surfaced = gate
            .run(roster_key(), || async {
                Err(ProjectionFailure {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    body: json!({ "error": "failed" }),
                })
            })
            .await;
        assert!(matches!(surfaced, Err(ProjectionGateError::Build(_))));
    }

    #[tokio::test]
    async fn stale_fallback_expires_after_max_age() {
        tokio::time::pause();
        let gate = ProjectionGate::new_with_stale_max_age(
            4,
            Duration::from_millis(250),
            Duration::from_millis(300),
        );

        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"first")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;
        let still_fresh = gate
            .run(roster_key(), || async {
                Err(service_unavailable_failure())
            })
            .await;
        match still_fresh {
            Err(ProjectionGateError::StaleServed(bytes)) => {
                assert_eq!(bytes, Bytes::from_static(b"first"))
            }
            other => panic!("expected stale fallback, got {:?}", other),
        }

        tokio::time::advance(Duration::from_millis(100)).await;
        let expired = gate
            .run(roster_key(), || async {
                Err(service_unavailable_failure())
            })
            .await;
        assert!(matches!(expired, Err(ProjectionGateError::Build(_))));

        // The expired entry is dropped so long-lived processes do not keep
        // per-key stale bytes forever; the expiration stays observable via
        // the stale_expired diagnostics counter.
        assert!(gate
            .state
            .lock()
            .expect("projection gate lock poisoned")
            .stale
            .is_empty());
        let snapshot = diagnostics::performance_snapshot();
        assert!(snapshot.projection_gate.stale_expired >= 1);
    }

    #[tokio::test]
    async fn stale_entries_for_other_keys_are_swept_on_insert() {
        tokio::time::pause();
        let gate = ProjectionGate::new_with_stale_max_age(
            4,
            Duration::from_millis(250),
            Duration::from_millis(300),
        );

        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"roster")) })
            .await
            .unwrap();
        // Advance beyond stale_max_age without touching the roster key again.
        tokio::time::advance(Duration::from_millis(400)).await;
        let expired_before = diagnostics::performance_snapshot()
            .projection_gate
            .stale_expired;

        // A successful build on a different key sweeps the expired roster
        // entry even though its key is never accessed again.
        gate.run(
            ProjectionKey::AgentProjectionSnapshot("agent-1".to_string(), "principal".to_string()),
            || async { Ok(Bytes::from_static(b"projection")) },
        )
        .await
        .unwrap();

        let state = gate.state.lock().expect("projection gate lock poisoned");
        assert!(!state.stale.contains_key(&roster_key()));
        assert!(state
            .stale
            .contains_key(&ProjectionKey::AgentProjectionSnapshot(
                "agent-1".to_string(),
                "principal".to_string()
            )));
        drop(state);
        let expired_after = diagnostics::performance_snapshot()
            .projection_gate
            .stale_expired;
        assert!(expired_after >= expired_before + 1);
    }

    #[tokio::test]
    async fn stale_delivery_test_counts_are_isolated_per_gate() {
        tokio::time::pause();
        let gate = ProjectionGate::new(1, Duration::from_millis(1));
        let other = ProjectionGate::new(1, Duration::from_millis(1));
        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"first")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(matches!(
            gate.run(roster_key(), || async {
                Err(service_unavailable_failure())
            })
            .await,
            Err(ProjectionGateError::StaleServed(_))
        ));
        assert_eq!(gate.stale_deliveries.load(Ordering::Relaxed), 1);
        assert_eq!(other.stale_deliveries.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn non_retryable_service_unavailable_failure_surfaces_instead_of_stale_fallback() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));

        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"first")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let served_before = gate.stale_deliveries.load(Ordering::Relaxed);
        let surfaced = gate
            .run(roster_key(), || async {
                Err(ProjectionFailure {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    body: json!({
                        "error": "runtime database is quarantined",
                        "code": "runtime_db_quarantined",
                        "retryable": false,
                    }),
                })
            })
            .await;
        match surfaced {
            Err(ProjectionGateError::Build(failure)) => {
                assert_eq!(failure.status, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(failure.error_code(), Some("runtime_db_quarantined"));
                assert_eq!(failure.retryable(), Some(false));
            }
            other => panic!(
                "expected the quarantine failure to surface, got {:?}",
                other
            ),
        }
        let served_after = gate.stale_deliveries.load(Ordering::Relaxed);
        assert_eq!(served_before, served_after);
    }

    #[tokio::test]
    async fn waiters_receive_stale_bytes_when_leader_fails_retryably() {
        tokio::time::pause();
        let gate = Arc::new(ProjectionGate::new(4, Duration::from_millis(250)));
        gate.run(roster_key(), || async { Ok(Bytes::from_static(b"first")) })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let leader = {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                gate.run(roster_key(), || async move {
                    started.notify_one();
                    release.notified().await;
                    Err(service_unavailable_failure())
                })
                .await
            })
        };
        started.notified().await;
        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                gate.run(roster_key(), || async {
                    Ok(Bytes::from_static(b"unexpected"))
                })
                .await
            })
        };
        tokio::task::yield_now().await;
        release.notify_one();

        for task in [leader, waiter] {
            match task.await.unwrap() {
                Err(ProjectionGateError::StaleServed(bytes)) => {
                    assert_eq!(bytes, Bytes::from_static(b"first"))
                }
                other => panic!("expected stale fallback, got {:?}", other),
            }
        }
    }
}
