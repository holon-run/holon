use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::Context,
};

use axum::{
    body::{to_bytes, Body, Bytes},
    http::{Request, Response, StatusCode},
    routing::get,
    Router,
};
use futures_util::task::noop_waker;
use tower::{Layer, Service, ServiceExt};
use tower_http::trace::TraceLayer;

use super::in_flight::InFlightLayer;
use super::redact_request_path;

#[test]
fn callback_paths_hide_capabilities() {
    assert_eq!(
        redact_request_path("/api/callbacks/wake/opaque"),
        "/api/callbacks/wake/<redacted>"
    );
    assert_eq!(
        redact_request_path("/api/callbacks/enqueue/opaque"),
        "/api/callbacks/enqueue/<redacted>"
    );
    assert_eq!(redact_request_path("/api/agents"), "/api/agents");
}

fn get_request(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

async fn never() {
    std::future::pending::<()>().await
}

/// Router mirroring production ordering: the in-flight layer wraps the route
/// and the trace layer wraps the in-flight layer.
fn counting_router(counter: Arc<AtomicUsize>) -> Router {
    Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route("/created", get(|| async { StatusCode::CREATED }))
        .route("/bad", get(|| async { StatusCode::BAD_REQUEST }))
        .route("/boom", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/pending", get(never))
        .route(
            "/stream",
            get(|| async {
                let chunks = futures_util::stream::iter(vec![Ok::<_, std::io::Error>(
                    Bytes::from_static(b"chunk"),
                )]);
                Response::new(Body::from_stream(chunks))
            }),
        )
        .route(
            "/upgrade",
            get(|| async {
                Response::builder()
                    .status(StatusCode::SWITCHING_PROTOCOLS)
                    .body(Body::empty())
                    .unwrap()
            }),
        )
        .layer(InFlightLayer::with_counter(counter))
        .layer(TraceLayer::new_for_http())
}

fn assert_returns_to(counter: &AtomicUsize, expected: usize) {
    assert_eq!(counter.load(Ordering::Relaxed), expected);
}

#[tokio::test]
async fn completed_request_returns_unit() {
    let counter = Arc::new(AtomicUsize::new(0));
    let response = counting_router(counter.clone())
        .oneshot(get_request("/ok"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn early_client_responses_return_unit() {
    let counter = Arc::new(AtomicUsize::new(0));
    // 4xx is not a server failure but still must return the unit once.
    let response = counting_router(counter.clone())
        .oneshot(get_request("/bad"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn server_error_response_returns_unit_once() {
    let counter = Arc::new(AtomicUsize::new(0));
    // 5xx triggers both `on_response` and `on_failure(Status)`; the counter
    // must still return exactly one unit, never two.
    let response = counting_router(counter.clone())
        .oneshot(get_request("/boom"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn cancelled_pending_request_returns_unit() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut future = Box::pin(counting_router(counter.clone()).oneshot(get_request("/pending")));
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert_returns_to(&counter, 1);
    // Dropping the response future simulates a client disconnect before the
    // response headers were produced.
    drop(future);
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn unpolled_request_future_returns_unit_on_drop() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut service = InFlightLayer::with_counter(counter.clone()).layer(tower::service_fn(
        |_req: Request<Body>| async {
            Ok::<Response<Body>, Infallible>(Response::new(Body::empty()))
        },
    ));
    let future = service.call(get_request("/"));
    // Accepting the request increments before the first poll.
    assert_returns_to(&counter, 1);
    drop(future);
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn service_error_returns_unit() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut service = InFlightLayer::with_counter(counter.clone()).layer(tower::service_fn(
        |_req: Request<Body>| async { Err::<Response<Body>, &'static str>("boom") },
    ));
    let result = service.call(get_request("/")).await;
    assert!(result.is_err());
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn streaming_body_does_not_change_counter_after_headers() {
    let counter = Arc::new(AtomicUsize::new(0));
    let response = counting_router(counter.clone())
        .oneshot(get_request("/stream"))
        .await
        .unwrap();
    // Unit returned when headers were ready, before the body streams.
    assert_returns_to(&counter, 0);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..], b"chunk");
    // Consuming the body must not touch the counter again.
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn switching_protocols_response_returns_unit_at_headers() {
    let counter = Arc::new(AtomicUsize::new(0));
    let response = counting_router(counter.clone())
        .oneshot(get_request("/upgrade"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    // 101 returned the unit at response headers; the later upgrade session does
    // not change the value.
    assert_returns_to(&counter, 0);
}

async fn poll_pending<F>(future: &mut Pin<Box<F>>, cx: &mut Context<'_>)
where
    F: Future,
{
    assert!(future.as_mut().poll(cx).is_pending());
}

#[tokio::test]
async fn two_in_flight_cancel_then_finish_forward_order() {
    let counter = Arc::new(AtomicUsize::new(0));
    let app = counting_router(counter.clone());
    let mut first = Box::pin(app.clone().oneshot(get_request("/pending")));
    let mut second = Box::pin(app.clone().oneshot(get_request("/pending")));
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    poll_pending(&mut first, &mut cx).await;
    poll_pending(&mut second, &mut cx).await;
    assert_returns_to(&counter, 2);

    // Cancelling the first returns only its own unit: B+2 -> B+1.
    drop(first);
    assert_returns_to(&counter, 1);
    // The second finishes only when its future is dropped: B+1 -> B.
    drop(second);
    assert_returns_to(&counter, 0);
}

#[tokio::test]
async fn two_in_flight_cancel_then_finish_reverse_order() {
    let counter = Arc::new(AtomicUsize::new(0));
    let app = counting_router(counter.clone());
    let mut first = Box::pin(app.clone().oneshot(get_request("/pending")));
    let mut second = Box::pin(app.clone().oneshot(get_request("/pending")));
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    poll_pending(&mut first, &mut cx).await;
    poll_pending(&mut second, &mut cx).await;
    assert_returns_to(&counter, 2);

    // Reverse order: the second (last accepted) is cancelled first.
    drop(second);
    assert_returns_to(&counter, 1);
    drop(first);
    assert_returns_to(&counter, 0);
}
