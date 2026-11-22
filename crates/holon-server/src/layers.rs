use std::time::Duration;

use axum::{
    extract::MatchedPath,
    http::{header::ETAG, HeaderName, HeaderValue, Method, Request as AxumRequest, Response},
    middleware::map_response,
    Router,
};
use holon_config::ApiCorsConfigFile;
use tower_http::{
    classify::ServerErrorsFailureClass,
    compression::CompressionLayer,
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::{info, warn, Span};

use crate::response::{add_retry_after_to_service_unavailable, is_slow_or_large_response};

mod in_flight;

use in_flight::InFlightLayer;

pub fn apply_http_layers<S>(router: Router<S>, cors: &ApiCorsConfigFile) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router
        .layer(api_cors_layer(cors))
        .layer(CompressionLayer::new())
        .layer(map_response(add_retry_after_to_service_unavailable))
        // In-flight accounting wraps the route but sits inside the trace layer,
        // so the request span is already entered when `call` accepts the
        // request and the response callbacks observe the returned unit.
        .layer(InFlightLayer::shared())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &AxumRequest<axum::body::Body>| {
                    let matched_path = request
                        .extensions()
                        .get::<MatchedPath>()
                        .map(MatchedPath::as_str)
                        .unwrap_or_else(|| request.uri().path());
                    // Metadata-only client attribution: a bounded user agent
                    // plus the observer principal recorded during
                    // authentication. Together they let journal records answer
                    // "which client polls which route" without process
                    // forensics; no request body is captured.
                    let user_agent = request
                        .headers()
                        .get(axum::http::header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                        .map(|value| value.chars().take(120).collect::<String>());
                    let span = tracing::info_span!(
                        target: "holon::http",
                        "http_request",
                        method = %request.method(),
                        path = %redact_request_path(request.uri().path()),
                        matched_path,
                        user_agent = tracing::field::Empty,
                        observer_principal = tracing::field::Empty,
                        status = tracing::field::Empty,
                        response_bytes = tracing::field::Empty,
                        elapsed_ms = tracing::field::Empty,
                        in_flight = tracing::field::Empty,
                    );
                    if let Some(user_agent) = user_agent {
                        span.record("user_agent", user_agent.as_str());
                    }
                    span
                })
                .on_response(
                    |response: &Response<axum::body::Body>,
                     elapsed: std::time::Duration,
                     span: &Span| {
                        // Response headers are ready: return the unit here and
                        // log the post-return value. Streamed bodies and
                        // upgrades no longer change the counter.
                        let in_flight = in_flight::current_http_in_flight_requests();
                        let response_bytes = response
                            .headers()
                            .get(axum::http::header::CONTENT_LENGTH)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<usize>().ok());
                        span.record("status", response.status().as_u16());
                        span.record("elapsed_ms", elapsed.as_millis() as u64);
                        span.record("in_flight", in_flight);
                        if let Some(bytes) = response_bytes {
                            span.record("response_bytes", bytes);
                        }

                        if is_slow_or_large_response(elapsed, response_bytes) {
                            warn!(
                                target: "holon::http",
                                status = response.status().as_u16(),
                                elapsed_ms = elapsed.as_millis() as u64,
                                response_bytes,
                                in_flight,
                                "slow or large HTTP response"
                            );
                        } else {
                            info!(
                                target: "holon::http",
                                status = response.status().as_u16(),
                                elapsed_ms = elapsed.as_millis() as u64,
                                response_bytes,
                                in_flight,
                                "HTTP response"
                            );
                        }
                    },
                )
                .on_failure(
                    |failure: ServerErrorsFailureClass,
                     elapsed: std::time::Duration,
                     span: &Span| {
                        if matches!(failure, ServerErrorsFailureClass::Error(_)) {
                            let in_flight = in_flight::current_http_in_flight_requests();
                            span.record("elapsed_ms", elapsed.as_millis() as u64);
                            span.record("in_flight", in_flight);
                            warn!(
                                target: "holon::http",
                                %failure,
                                elapsed_ms = elapsed.as_millis() as u64,
                                in_flight,
                                "HTTP request failed before response"
                            );
                        }
                    },
                ),
        )
}

fn api_cors_layer(config: &ApiCorsConfigFile) -> CorsLayer {
    if !config.enabled() {
        return CorsLayer::new();
    }

    let methods = config
        .allowed_methods
        .iter()
        .filter_map(|method| method.parse::<Method>().ok())
        .collect::<Vec<_>>();
    let headers = config
        .allowed_headers
        .iter()
        .filter_map(|header| header.parse::<HeaderName>().ok())
        .collect::<Vec<_>>();

    let allow_origin = if config.allowed_origins.iter().any(|origin| origin == "*") {
        AllowOrigin::any()
    } else {
        let configured_origins = config
            .allowed_origins
            .iter()
            .filter_map(|origin| origin.parse::<HeaderValue>().ok())
            .collect::<Vec<_>>();
        AllowOrigin::predicate(move |origin, _| {
            is_default_localhost_cors_origin(origin) || configured_origins.contains(origin)
        })
    };

    let mut layer = CorsLayer::new()
        .allow_origin(allow_origin)
        // Conditional conversation revalidation reads the ETag header; without
        // exposing it, cross-origin clients silently degrade to full 200s.
        .expose_headers([
            ETAG,
            HeaderName::from_static("x-holon-agent-id"),
            HeaderName::from_static("x-holon-agent-incarnation"),
            HeaderName::from_static(holon_domain::runtime_event::EVENT_CONTRACT_VERSION_HEADER),
        ])
        .allow_methods(methods)
        .allow_headers(headers)
        .max_age(Duration::from_secs(config.max_age_seconds()));

    if config.allow_credentials() {
        layer = layer.allow_credentials(true);
    }

    layer
}

fn is_default_localhost_cors_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin) = url::Url::parse(origin) else {
        return false;
    };
    if !matches!(origin.scheme(), "http" | "https") {
        return false;
    }
    if origin.path() != "/" || origin.query().is_some() || origin.fragment().is_some() {
        return false;
    }
    match origin.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(addr)) => addr == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(addr)) => addr == std::net::Ipv6Addr::LOCALHOST,
        None => false,
    }
}
fn redact_request_path(path: &str) -> String {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() == 5
        && parts[1] == "api"
        && parts[2] == "callbacks"
        && matches!(parts[3], "enqueue" | "wake")
    {
        return format!("/api/callbacks/{}/<redacted>", parts[3]);
    }
    path.to_string()
}

#[cfg(test)]
mod tests;
