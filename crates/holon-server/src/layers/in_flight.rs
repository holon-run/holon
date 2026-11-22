//! HTTP in-flight request accounting.
//!
//! Every accepted request takes exactly one unit in [`InFlightService::call`]
//! and must return that unit exactly once. The unit is returned when response
//! headers become ready (the wrapped service future resolves), when the service
//! returns an error, or when the request future is dropped before producing a
//! response (client cancellation). Streamed bodies and protocol upgrades happen
//! after the unit is returned and must not change the counter again.
//!
//! The counter lifetime lives in [`InFlightToken`] and is guarded by
//! [`InFlightGuard`], so correctness never depends on whether tracing logs are
//! enabled.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    task::{Context, Poll},
};

use axum::http::Request;
use futures_util::future::BoxFuture;
use tower::{Layer, Service};
use tracing::Span;

/// Process-wide counter backing the HTTP in-flight gauge.
pub(crate) fn http_in_flight_counter() -> Arc<AtomicUsize> {
    static COUNTER: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    COUNTER
        .get_or_init(|| Arc::new(AtomicUsize::new(0)))
        .clone()
}

/// Current process-wide in-flight request count, for response/error logging.
pub(crate) fn current_http_in_flight_requests() -> usize {
    http_in_flight_counter().load(Ordering::Relaxed)
}

/// One accepted request's return obligation.
#[derive(Debug)]
pub(crate) struct InFlightToken {
    counter: Arc<AtomicUsize>,
    /// Counter value including this request, captured when it was accepted.
    pub(crate) accepted_in_flight: usize,
    returned: AtomicBool,
}

impl InFlightToken {
    fn accept(counter: Arc<AtomicUsize>) -> Arc<Self> {
        let accepted_in_flight = counter.fetch_add(1, Ordering::Relaxed) + 1;
        Arc::new(Self {
            counter,
            accepted_in_flight,
            returned: AtomicBool::new(false),
        })
    }

    /// Returns this request's unit exactly once.
    ///
    /// The first call decrements the counter and returns the post-return value;
    /// every later call is a no-op returning `None`, so a double decrement
    /// cannot be masked by later requests finishing.
    pub(crate) fn return_once(&self) -> Option<usize> {
        if self.returned.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(decrement(&self.counter))
    }
}

fn decrement(counter: &AtomicUsize) -> usize {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        if current == 0 {
            return 0;
        }
        match counter.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current - 1,
            Err(next) => current = next,
        }
    }
}

/// Returns the request's unit when dropped, covering cancellation paths that
/// never reach a response or error callback.
struct InFlightGuard {
    token: Arc<InFlightToken>,
}

impl InFlightGuard {
    /// Explicitly returns the unit at a well-defined point (headers ready).
    /// The guard's `Drop` remains idempotent afterwards.
    fn return_now(self) {
        self.token.return_once();
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.token.return_once();
    }
}

/// Tower layer that owns the per-request in-flight accounting.
#[derive(Clone)]
pub(crate) struct InFlightLayer {
    counter: Arc<AtomicUsize>,
}

impl InFlightLayer {
    pub(crate) fn shared() -> Self {
        Self {
            counter: http_in_flight_counter(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_counter(counter: Arc<AtomicUsize>) -> Self {
        Self { counter }
    }
}

impl<S> Layer<S> for InFlightLayer {
    type Service = InFlightService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InFlightService {
            inner,
            counter: self.counter.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct InFlightService<S> {
    inner: S,
    counter: Arc<AtomicUsize>,
}

impl<S, B> Service<Request<B>> for InFlightService<S>
where
    S: Service<Request<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
    S::Error: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let token = InFlightToken::accept(self.counter.clone());
        // This layer sits inside the trace layer, whose request span is entered
        // while `call` runs, so the start signal reports the post-increment
        // value on the same span the response callbacks use.
        Span::current().record("in_flight", token.accepted_in_flight);
        let guard = InFlightGuard { token };
        // Keep a ready service in place while the request future runs.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let result = inner.call(request).await;
            // Headers are ready (or the service errored): return the unit at
            // this deterministic point rather than waiting for body completion.
            guard.return_now();
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_returns_exactly_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let token = InFlightToken::accept(counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        assert_eq!(token.accepted_in_flight, 1);

        assert_eq!(token.return_once(), Some(0));
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        // A second return must not push the counter negative or decrement an
        // unrelated request that is still in flight.
        assert_eq!(token.return_once(), None);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn two_requests_return_independently() {
        let counter = Arc::new(AtomicUsize::new(5));
        let first = InFlightToken::accept(counter.clone());
        let second = InFlightToken::accept(counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 7);
        assert_eq!(first.accepted_in_flight, 6);
        assert_eq!(second.accepted_in_flight, 7);

        // Cancelling one request returns only its own unit: B+2 -> B+1.
        assert_eq!(first.return_once(), Some(6));
        assert_eq!(counter.load(Ordering::Relaxed), 6);
        // Finishing the other request returns the remaining unit: B+1 -> B.
        assert_eq!(second.return_once(), Some(5));
        assert_eq!(counter.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn dropping_guard_returns_unit_once() {
        let counter = Arc::new(AtomicUsize::new(0));
        let token = InFlightToken::accept(counter.clone());
        {
            let _guard = InFlightGuard {
                token: token.clone(),
            };
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        // A later guard drop is a no-op, so the counter stays at zero.
        {
            let _guard = InFlightGuard {
                token: token.clone(),
            };
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn explicit_return_prevents_double_decrement_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let token = InFlightToken::accept(counter.clone());
        let guard = InFlightGuard {
            token: token.clone(),
        };
        assert_eq!(token.return_once(), Some(0));
        drop(guard);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
