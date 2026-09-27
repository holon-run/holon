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

const DEFAULT_MAX_LEADERS: usize = crate::config::DEFAULT_API_PROJECTION_MAX_LEADERS as usize;
const DEFAULT_TTL: Duration =
    Duration::from_millis(crate::config::DEFAULT_API_PROJECTION_CACHE_TTL_MS);
/// How long the last successful build for a projection key may be served as
/// a stale fallback after a retryable (service-unavailable) assembly
/// failure. Projection assembly budgets are ten seconds; one fallback window
/// covers client retries through a saturation burst without serving
/// arbitrarily old projections.
pub(crate) const PROJECTION_STALE_FALLBACK_MAX_AGE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ProjectionKey {
    AgentsList(Option<String>),
    AgentsRosterSnapshot,
    AgentProjectionSnapshot(String),
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
}

impl From<(StatusCode, Json<Value>)> for ProjectionFailure {
    fn from((status, Json(body)): (StatusCode, Json<Value>)) -> Self {
        Self { status, body }
    }
}

#[derive(Debug)]
pub(crate) enum ProjectionGateError {
    Build(ProjectionFailure),
    /// The assembly failed retryably and a fresh-enough last good projection
    /// exists. Carries the stale bytes so the caller serves them with the
    /// stale marker instead of a bare success the client would read as fresh.
    StaleServed(Bytes),
    Rejected,
}

type ProjectionResult = Result<Bytes, ProjectionFailure>;

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
    Finished(ProjectionResult),
}

#[derive(Debug)]
struct Flight {
    state: watch::Sender<FlightState>,
}

#[derive(Debug)]
enum Entry {
    InFlight { flight: Arc<Flight> },
    Ready { bytes: Bytes, expires_at: Instant },
}

#[derive(Debug)]
struct StaleEntry {
    bytes: Bytes,
    built_at: Instant,
}

#[derive(Debug)]
pub(crate) struct ProjectionGate {
    entries: Mutex<HashMap<ProjectionKey, Entry>>,
    stale: Mutex<HashMap<ProjectionKey, StaleEntry>>,
    leaders: Arc<Semaphore>,
    ttl: Duration,
    stale_max_age: Duration,
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
            entries: Mutex::new(HashMap::new()),
            stale: Mutex::new(HashMap::new()),
            leaders: Arc::new(Semaphore::new(max_leaders)),
            ttl,
            stale_max_age,
        }
    }

    pub(crate) async fn run<F, Fut>(
        &self,
        key: ProjectionKey,
        build: F,
    ) -> Result<Bytes, ProjectionGateError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = ProjectionResult>,
    {
        let fallback_key = key.clone();
        enum Decision<'a> {
            Ready(Bytes),
            Wait(Arc<Flight>),
            Lead(LeaderGuard<'a>),
            Reject,
        }

        let decision = {
            let mut entries = self.entries.lock().expect("projection gate lock poisoned");
            let now = Instant::now();
            match entries.get(&key) {
                Some(Entry::Ready { bytes, expires_at }) if *expires_at > now => {
                    diagnostics::record_projection_gate_cache_hit();
                    Decision::Ready(bytes.clone())
                }
                Some(Entry::InFlight { flight }) => {
                    diagnostics::record_projection_gate_cache_miss();
                    diagnostics::record_projection_gate_joined_waiter();
                    Decision::Wait(Arc::clone(flight))
                }
                _ => {
                    entries.retain(|_, entry| {
                        !matches!(
                            entry,
                            Entry::Ready { expires_at, .. } if *expires_at <= now
                        )
                    });
                    diagnostics::record_projection_gate_cache_miss();
                    match Arc::clone(&self.leaders).try_acquire_owned() {
                        Ok(permit) => {
                            let (state, _) = watch::channel(FlightState::Pending);
                            let flight = Arc::new(Flight { state });
                            entries.insert(
                                key.clone(),
                                Entry::InFlight {
                                    flight: Arc::clone(&flight),
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
                let waited = wait_for_flight(flight).await;
                self.settle_retryable_failure(&fallback_key, waited)
            }
            Decision::Lead(mut guard) => {
                let result = build().await;
                if result.is_err() {
                    diagnostics::record_projection_gate_failed();
                }
                guard.finish(result.clone());
                self.settle_retryable_failure(&fallback_key, result)
            }
            Decision::Reject => Err(ProjectionGateError::Rejected),
        }
    }

    /// A retryable assembly failure (service-unavailable class, e.g. the
    /// roster snapshot budget timeout or a cancelled leader) still has a
    /// usable answer when this key recently produced a fresh projection:
    /// serve the last good bytes instead of failing the request. The
    /// in-flight entry stays released, so the next caller rebuilds.
    fn settle_retryable_failure(
        &self,
        key: &ProjectionKey,
        result: ProjectionResult,
    ) -> Result<Bytes, ProjectionGateError> {
        if let Err(failure) = &result {
            if is_retryable_service_unavailable(failure) {
                if let Some(bytes) = self.fresh_stale_bytes(key) {
                    tracing::warn!(
                        status = %failure.status,
                        "projection assembly failed retryably; serving the last good projection marked stale"
                    );
                    diagnostics::record_projection_gate_stale_served();
                    return Err(ProjectionGateError::StaleServed(bytes));
                }
            }
        }
        result.map_err(ProjectionGateError::Build)
    }

    fn fresh_stale_bytes(&self, key: &ProjectionKey) -> Option<Bytes> {
        let mut stale = self.stale.lock().expect("projection gate lock poisoned");
        match stale.get(key) {
            Some(entry) if entry.built_at.elapsed() <= self.stale_max_age => {
                Some(entry.bytes.clone())
            }
            Some(_) => {
                // Expired fallback entries have no other cleanup path; drop
                // them on access so long-lived processes do not accumulate
                // per-key stale bytes without bound.
                stale.remove(key);
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
    _permit: OwnedSemaphorePermit,
    finished: bool,
}

impl<'a> LeaderGuard<'a> {
    fn new(
        gate: &'a ProjectionGate,
        key: ProjectionKey,
        flight: Arc<Flight>,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        diagnostics::record_projection_gate_leader_started();
        Self {
            gate,
            key,
            flight,
            _permit: permit,
            finished: false,
        }
    }

    fn finish(&mut self, result: ProjectionResult) {
        {
            let mut entries = self
                .gate
                .entries
                .lock()
                .expect("projection gate lock poisoned");
            if entry_matches_flight(entries.get(&self.key), &self.flight) {
                match &result {
                    Ok(bytes) => {
                        let mut stale = self
                            .gate
                            .stale
                            .lock()
                            .expect("projection gate lock poisoned");
                        // Sweep expired entries on insert. Cleanup on access
                        // alone would let never-revisited keys (request-derived
                        // per-agent projections) accumulate stale bytes without
                        // bound in long-lived processes.
                        let built_at = Instant::now();
                        let max_age = self.gate.stale_max_age;
                        let before = stale.len();
                        stale.retain(|_, entry| entry.built_at.elapsed() <= max_age);
                        diagnostics::record_projection_gate_stale_expired_by(
                            (before - stale.len()) as u64,
                        );
                        stale.insert(
                            self.key.clone(),
                            StaleEntry {
                                bytes: bytes.clone(),
                                built_at,
                            },
                        );
                        drop(stale);
                        entries.insert(
                            self.key.clone(),
                            Entry::Ready {
                                bytes: bytes.clone(),
                                expires_at: Instant::now() + self.gate.ttl,
                            },
                        );
                    }
                    Err(_) => {
                        entries.remove(&self.key);
                    }
                }
            }
        }
        self.flight
            .state
            .send_replace(FlightState::Finished(result));
        self.finished = true;
    }
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let failure = ProjectionFailure::leader_cancelled();
            {
                let mut entries = self
                    .gate
                    .entries
                    .lock()
                    .expect("projection gate lock poisoned");
                if entry_matches_flight(entries.get(&self.key), &self.flight) {
                    entries.remove(&self.key);
                }
            }
            self.flight
                .state
                .send_replace(FlightState::Finished(Err(failure)));
            diagnostics::record_projection_gate_cancelled();
        }
        diagnostics::record_projection_gate_leader_finished();
    }
}

fn entry_matches_flight(entry: Option<&Entry>, flight: &Arc<Flight>) -> bool {
    matches!(
        entry,
        Some(Entry::InFlight { flight: current }) if Arc::ptr_eq(current, flight)
    )
}

async fn wait_for_flight(flight: Arc<Flight>) -> ProjectionResult {
    let mut state = flight.state.subscribe();
    loop {
        if let FlightState::Finished(result) = state.borrow().clone() {
            return result;
        }
        if state.changed().await.is_err() {
            return Err(ProjectionFailure::leader_cancelled());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;

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

        let entries = gate.entries.lock().unwrap();
        assert!(!entries.contains_key(&ProjectionKey::AgentState("removed-agent".into())));
        assert!(entries.contains_key(&ProjectionKey::AgentsList(None)));
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
    async fn retryable_failure_serves_last_good_bytes_and_releases_the_key() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));
        let builds = AtomicUsize::new(0);

        let first = gate
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(Bytes::from_static(b"first"))
            })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let stale = gate
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
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
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
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

        gate.run(ProjectionKey::AgentsRosterSnapshot, || async {
            Ok(Bytes::from_static(b"first"))
        })
        .await
        .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let surfaced = gate
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
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

        gate.run(ProjectionKey::AgentsRosterSnapshot, || async {
            Ok(Bytes::from_static(b"first"))
        })
        .await
        .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;
        let still_fresh = gate
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
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
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
                Err(service_unavailable_failure())
            })
            .await;
        assert!(matches!(expired, Err(ProjectionGateError::Build(_))));

        // The expired entry is dropped so long-lived processes do not keep
        // per-key stale bytes forever; the expiration stays observable via
        // the stale_expired diagnostics counter.
        assert!(gate
            .stale
            .lock()
            .expect("projection gate lock poisoned")
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

        gate.run(ProjectionKey::AgentsRosterSnapshot, || async {
            Ok(Bytes::from_static(b"roster"))
        })
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
            ProjectionKey::AgentProjectionSnapshot("agent-1".to_string()),
            || async { Ok(Bytes::from_static(b"projection")) },
        )
        .await
        .unwrap();

        let stale = gate.stale.lock().expect("projection gate lock poisoned");
        assert!(!stale.contains_key(&ProjectionKey::AgentsRosterSnapshot));
        assert!(stale.contains_key(&ProjectionKey::AgentProjectionSnapshot(
            "agent-1".to_string()
        )));
        drop(stale);
        let expired_after = diagnostics::performance_snapshot()
            .projection_gate
            .stale_expired;
        assert!(expired_after >= expired_before + 1);
    }

    #[tokio::test]
    async fn non_retryable_service_unavailable_failure_surfaces_instead_of_stale_fallback() {
        tokio::time::pause();
        let gate = ProjectionGate::new(4, Duration::from_millis(250));

        gate.run(ProjectionKey::AgentsRosterSnapshot, || async {
            Ok(Bytes::from_static(b"first"))
        })
        .await
        .unwrap();
        tokio::time::advance(Duration::from_millis(251)).await;

        let served_before = diagnostics::performance_snapshot()
            .projection_gate
            .stale_served;
        let surfaced = gate
            .run(ProjectionKey::AgentsRosterSnapshot, || async {
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
            }
            other => panic!(
                "expected the quarantine failure to surface, got {:?}",
                other
            ),
        }
        let served_after = diagnostics::performance_snapshot()
            .projection_gate
            .stale_served;
        assert_eq!(served_before, served_after);
    }

    #[tokio::test]
    async fn waiters_receive_stale_bytes_when_leader_fails_retryably() {
        tokio::time::pause();
        let gate = Arc::new(ProjectionGate::new(4, Duration::from_millis(250)));
        gate.run(ProjectionKey::AgentsRosterSnapshot, || async {
            Ok(Bytes::from_static(b"first"))
        })
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
                gate.run(ProjectionKey::AgentsRosterSnapshot, || async move {
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
                gate.run(ProjectionKey::AgentsRosterSnapshot, || async {
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
