use axum::http::{HeaderName, HeaderValue, Method};
use tokio::time::Duration;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::config::ApiCorsConfigFile;

pub(super) fn api_cors_layer(config: &ApiCorsConfigFile) -> CorsLayer {
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
        .allow_methods(methods)
        .allow_headers(headers)
        .max_age(Duration::from_secs(config.max_age_seconds()));

    if config.allow_credentials() {
        layer = layer.allow_credentials(true);
    }

    layer
}

pub(super) fn is_default_localhost_cors_origin(origin: &HeaderValue) -> bool {
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
