use axum::http::{HeaderMap, HeaderName};

const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const REFERER: HeaderName = HeaderName::from_static("referer");
const HOST: HeaderName = HeaderName::from_static("host");
const ORIGIN: HeaderName = HeaderName::from_static("origin");

pub fn browser_origin_allowed(headers: &HeaderMap, trusted_origins: &[String]) -> bool {
    let Some(origin) = single_header_value(headers, &ORIGIN) else {
        if headers
            .get(&SEC_FETCH_SITE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
        {
            return true;
        }

        let Some(referer) = single_header_value(headers, &REFERER) else {
            return false;
        };
        return origin_value_allowed(referer, headers, trusted_origins, true);
    };
    origin_value_allowed(origin, headers, trusted_origins, false)
}

fn single_header_value<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

fn origin_value_allowed(
    value: &str,
    headers: &HeaderMap,
    trusted_origins: &[String],
    allow_referer_path: bool,
) -> bool {
    let Ok(mut origin) = url::Url::parse(value) else {
        return false;
    };
    if !is_valid_http_source(&origin) || (!allow_referer_path && !is_valid_http_origin(&origin)) {
        return false;
    }
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    if origin_matches_request(&origin, headers) {
        return true;
    }
    trusted_origins
        .iter()
        .filter_map(|trusted| url::Url::parse(trusted).ok())
        .any(|trusted| origins_equal(&origin, &trusted))
}

fn is_valid_http_source(origin: &url::Url) -> bool {
    matches!(origin.scheme(), "http" | "https")
        && origin.host().is_some()
        && origin.username().is_empty()
        && origin.password().is_none()
}

fn is_valid_http_origin(origin: &url::Url) -> bool {
    is_valid_http_source(origin)
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
}

fn origins_equal(left: &url::Url, right: &url::Url) -> bool {
    is_valid_http_origin(left)
        && is_valid_http_origin(right)
        && left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.port_or_known_default() == right.port_or_known_default()
}

fn origin_matches_request(origin: &url::Url, headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(&HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(request_host) = url::Url::parse(&format!("http://{host}")) else {
        return false;
    };
    if !is_valid_http_origin(&request_host) {
        return false;
    }
    let Some(origin_host) = origin.host_str() else {
        return false;
    };
    let Some(request_host_value) = request_host.host_str() else {
        return false;
    };
    origin_host.eq_ignore_ascii_case(request_host_value)
        && origin.scheme().eq_ignore_ascii_case(request_host.scheme())
        && origin.port_or_known_default() == request_host.port_or_known_default()
}

pub fn desktop_origin_allowed(
    headers: &HeaderMap,
    mutation: bool,
    authenticated_remote: bool,
) -> bool {
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Ok(url) = url::Url::parse(&format!("http://{host}")) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.authority() != host
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    // Local mode without credentials must not trust a DNS-rebound Host.
    if !authenticated_remote
        && !match url.host() {
            Some(url::Host::Domain(host)) => {
                let host = host.trim_end_matches('.');
                host == "localhost" || host.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => {
                ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
            }
            None => false,
        }
    {
        return false;
    }
    if headers
        .get("sec-fetch-site")
        .is_some_and(|v| v != "same-origin")
    {
        return false;
    }
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        Some(origin) => url::Url::parse(origin).is_ok_and(|origin_url| {
            matches!(origin_url.scheme(), "http" | "https")
                && origin_url.authority() == host
                && origin_url.origin().ascii_serialization() == origin
        }),
        None => !mutation,
    }
}
