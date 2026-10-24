use anyhow::Result;
use reqwest::{RequestBuilder, Response};

use super::{http_timeouts, response_body_timeout, stream_idle_timeout};
use crate::provider::{
    http_trace::ProviderHttpTraceRequest,
    retry::{classify_reqwest_transport_error_with_trace, timeout_transport_error_with_trace},
    ProviderDispatchGuard,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct DispatchGuardRejected(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequestDeadline {
    FullRequest,
    HeadersOnly,
    TextStream,
}

#[cfg(test)]
mod tests {
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
                        .write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 10\r\n\r\n",
                        )
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
                assert!(read_error_response_body(response, Some(&trace))
                    .await
                    .is_empty());
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
}

pub(super) async fn read_error_response_body(
    response: Response,
    trace: Option<&ProviderHttpTraceRequest>,
) -> String {
    match tokio::time::timeout(response_body_timeout(), response.text()).await {
        Ok(Ok(body)) => {
            if let Some(trace) = trace {
                trace.write_response_body(&body);
            }
            body
        }
        // Keep status-based retry semantics, without claiming the body completed.
        _ => String::new(),
    }
}

pub(super) async fn send_provider_request(
    request: RequestBuilder,
    context: &str,
    stage: &str,
    provider: &str,
    model_ref: Option<&str>,
    url: Option<&str>,
    deadline: RequestDeadline,
    trace: Option<&ProviderHttpTraceRequest>,
    dispatch_guard: Option<&dyn ProviderDispatchGuard>,
) -> Result<Response> {
    let timeouts = http_timeouts();
    let headers = match deadline {
        RequestDeadline::TextStream => timeouts.text_headers,
        RequestDeadline::FullRequest | RequestDeadline::HeadersOnly => timeouts.request,
    };
    if let Some(trace) = trace {
        trace.write_timeout_policy(
            timeouts.connect,
            headers,
            timeouts.body,
            stream_idle_timeout(),
            if deadline == RequestDeadline::FullRequest {
                Some(timeouts.request)
            } else {
                timeouts.total
            },
        );
    }
    let request = if deadline == RequestDeadline::FullRequest {
        request.timeout(timeouts.request)
    } else {
        request
    };
    let (client, request) = request.build_split();
    let request = request.map_err(|error| {
        classify_reqwest_transport_error_with_trace(
            context, stage, provider, model_ref, url, error, trace,
        )
    })?;
    if let Some(guard) = dispatch_guard {
        // Keep the original error downcastable, without classifying it as transport failure.
        guard.before_dispatch().map_err(|error| {
            let rejection = DispatchGuardRejected(error.to_string());
            error.context(rejection)
        })?;
    }
    if deadline == RequestDeadline::FullRequest {
        return client.execute(request).await.map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                context, stage, provider, model_ref, url, error, trace,
            )
        });
    }
    // RequestBuilder::timeout would continue through the entire stream body.
    tokio::time::timeout(headers, client.execute(request))
        .await
        .map_err(|_| {
            timeout_transport_error_with_trace(
                context,
                stage,
                provider,
                model_ref,
                url,
                format!("response_headers_timeout_ms={}", headers.as_millis()),
                trace,
            )
        })?
        .map_err(|error| {
            classify_reqwest_transport_error_with_trace(
                context, stage, provider, model_ref, url, error, trace,
            )
        })
}
