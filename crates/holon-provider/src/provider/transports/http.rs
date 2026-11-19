use anyhow::Result;
use reqwest::{RequestBuilder, Response};

use super::{http_timeouts, response_body_timeout, stream_idle_timeout};
use crate::provider::{
    http_trace::ProviderHttpTraceRequest,
    retry::{classify_reqwest_transport_error_with_trace, timeout_transport_error_with_trace},
    ProviderDispatchGuard, ProviderRequestMaterial,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct DispatchGuardRejected(String);

pub fn is_dispatch_guard_rejected(error: &anyhow::Error) -> bool {
    error.is::<DispatchGuardRejected>()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequestDeadline {
    FullRequest,
    HeadersOnly,
    TextStream,
}

#[cfg(test)]
mod tests;

pub(super) async fn read_error_response_body(
    response: Response,
    trace: Option<&ProviderHttpTraceRequest>,
    material: &ProviderRequestMaterial,
) -> Result<String> {
    match tokio::time::timeout(response_body_timeout(), material.response_text(response)).await {
        Ok(Ok(body)) => {
            if let Some(trace) = trace {
                trace.write_response_body(&body);
            }
            Ok(body)
        }
        Ok(Err(error)) if error.downcast_ref::<reqwest::Error>().is_none() => {
            Err(material.hold_error(error))
        }
        // Keep status-based retry semantics, without claiming the body completed.
        _ => Ok(String::new()),
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
