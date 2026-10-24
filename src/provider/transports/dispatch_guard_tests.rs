use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use reqwest::{RequestBuilder, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{
    build_http_client,
    http::{send_provider_request, RequestDeadline},
};
use crate::provider::{
    retry::{classify_provider_error, ProviderTransportError, RetryDisposition},
    ProviderDispatchGuard,
};

#[derive(Debug, thiserror::Error)]
#[error("test dispatch denied")]
struct GuardDenied;

#[derive(Debug, Default)]
struct CountingGuard {
    calls: AtomicUsize,
    reject: bool,
}

impl ProviderDispatchGuard for CountingGuard {
    fn before_dispatch(&self) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.reject {
            return Err(GuardDenied.into());
        }
        Ok(())
    }
}

const DEADLINES: [RequestDeadline; 3] = [
    RequestDeadline::FullRequest,
    RequestDeadline::HeadersOnly,
    RequestDeadline::TextStream,
];

async fn send(
    request: RequestBuilder,
    deadline: RequestDeadline,
    guard: &dyn ProviderDispatchGuard,
) -> anyhow::Result<Response> {
    send_provider_request(
        request,
        "test request failed",
        "request_send",
        "test",
        None,
        None,
        deadline,
        None,
        Some(guard),
    )
    .await
}

async fn assert_no_connection(listener: &TcpListener) {
    assert!(
        tokio::time::timeout(Duration::from_millis(25), listener.accept())
            .await
            .is_err(),
        "dispatch must not open a connection"
    );
}

#[tokio::test]
async fn dispatch_guard_not_called_for_build_failure() {
    let client = build_http_client().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let guard = CountingGuard::default();
    for deadline in DEADLINES {
        let error = send(
            client.post(&url).header("x-test", "invalid\nheader"),
            deadline,
            &guard,
        )
        .await
        .unwrap_err();
        assert!(error.is::<ProviderTransportError>());
        assert_eq!(guard.calls.load(Ordering::SeqCst), 0);
    }
    assert_no_connection(&listener).await;
}

#[tokio::test]
async fn dispatch_guard_called_once_per_http_send() {
    let client = build_http_client().unwrap();
    let guard = Arc::new(CountingGuard::default());
    for (index, deadline) in DEADLINES.into_iter().enumerate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let expected_calls = index + 1;
        let server_guard = Arc::clone(&guard);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(socket.read(&mut bytes).await.unwrap() > 0);
            assert_eq!(server_guard.calls.load(Ordering::SeqCst), expected_calls);
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let response = send(client.post(url), deadline, guard.as_ref())
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert_eq!(guard.calls.load(Ordering::SeqCst), expected_calls);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn dispatch_guard_rejection_preserves_type_without_network() {
    let client = build_http_client().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let guard = CountingGuard {
        reject: true,
        ..CountingGuard::default()
    };
    for (index, deadline) in DEADLINES.into_iter().enumerate() {
        let error = send(client.post(&url), deadline, &guard).await.unwrap_err();
        assert!(error.is::<GuardDenied>());
        assert!(!error.is::<ProviderTransportError>());
        assert_eq!(
            classify_provider_error(&error).disposition,
            RetryDisposition::FailFast
        );
        assert_eq!(guard.calls.load(Ordering::SeqCst), index + 1);
    }
    assert_no_connection(&listener).await;
}

#[tokio::test]
async fn dispatch_guard_does_not_follow_redirect() {
    let client = build_http_client().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirected = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let location = format!("http://{}/", redirected.local_addr().unwrap());
    let guard = CountingGuard::default();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(socket.read(&mut bytes).await.unwrap() > 0);
        socket
            .write_all(
                format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let response = send(client.post(url), RequestDeadline::FullRequest, &guard)
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(guard.calls.load(Ordering::SeqCst), 1);
    server.await.unwrap();
    assert_no_connection(&redirected).await;
}
