use super::*;
use crate::provider::{
    http_trace::ProviderHttpTrace,
    transports::{build_http_client, HttpTimeouts, HTTP_TIMEOUTS_OVERRIDE},
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn phase_timeout_error_body_trace_does_not_claim_body_completed() {
    let timeouts = HttpTimeouts {
        connect: Duration::from_secs(1),
        request: Duration::from_secs(1),
        text_headers: Duration::from_secs(1),
        body: Duration::from_millis(50),
        idle: Duration::from_secs(1),
        total: None,
    };
    HTTP_TIMEOUTS_OVERRIDE
        .scope(timeouts, async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let bytes_read = socket.read(&mut request).await.unwrap();
                assert!(bytes_read > 0, "expected request bytes before responding");
                socket
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 10\r\n\r\n")
                    .await
                    .unwrap();
                std::future::pending::<()>().await;
            });
            let home = tempfile::tempdir().unwrap();
            let trace = ProviderHttpTrace::failure_only_for_tests(home.path())
                .begin_request(
                    None,
                    "test",
                    None,
                    &url,
                    "test",
                    &[],
                    &serde_json::json!({}),
                )
                .unwrap();
            let response = build_http_client().unwrap().get(url).send().await.unwrap();
            trace.write_response_headers(response.status(), response.headers());
            assert!(
                read_error_response_body(response, Some(&trace), &Default::default())
                    .await
                    .unwrap()
                    .is_empty()
            );
            let path = trace.diagnostics(Some(503)).unwrap().path;
            let events = std::fs::read_to_string(path).unwrap();
            let summary = events
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .find(|event| event["type"] == "response_phase_summary")
                .unwrap();
            assert_eq!(summary["phase"], "response_headers");
            assert!(summary["body_read_completed_elapsed_ms"].is_null());
            assert!(summary["first_body_chunk_elapsed_ms"].is_null());
            server.abort();
        })
        .await;
}
