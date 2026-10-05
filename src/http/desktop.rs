//! Explicit host desktop integration. File identity uses the browsing resolver.
use super::*;

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DesktopCapabilities {
    reveal_in_finder: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevealFileRequest {
    workspace_id: String,
    execution_root_id: String,
    path: String,
}

fn same_origin_request(headers: &HeaderMap, mutation: bool, authenticated_remote: bool) -> bool {
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

pub(crate) async fn capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<DesktopCapabilities>, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    Ok(Json(DesktopCapabilities {
        reveal_in_finder: state.desktop_integration,
    }))
}

pub(crate) async fn reveal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RevealFileRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let authenticated_remote = state.require_control_token
        || state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc;
    if !state.desktop_integration || !same_origin_request(&headers, true, authenticated_remote) {
        return Err(forbidden(
            "desktop integration is unavailable for this connection",
        ));
    }
    let root = workspace_files::resolve_workspace_root(
        &state,
        &request.workspace_id,
        Some(&request.execution_root_id),
    )?;
    let path = workspace_files::resolve_and_validate_path(&root.filesystem_path, &request.path)?;
    let path = std::fs::canonicalize(path).map_err(|_| not_found("file is unavailable"))?;
    let canonical_root =
        std::fs::canonicalize(&root.filesystem_path).map_err(|err| error_response(err.into()))?;
    if !path.starts_with(canonical_root) {
        return Err(forbidden("path escapes workspace root"));
    }
    // Fixed executable and argument vector, never a shell or client-provided absolute path.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| error_response(anyhow!("Finder did not respond")))?
    .map_err(|err| error_response(err.into()))?;
    if !result.status.success() {
        return Err(error_response(anyhow!("could not reveal file in Finder")));
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_requires_same_origin_without_restricting_host_location() {
        for host in [
            "localhost:7878",
            "127.0.0.1:7878",
            "[::1]:7878",
            "192.0.2.5:7878",
            "host.tailnet.ts.net",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            assert!(same_origin_request(&headers, false, true));
            assert!(!same_origin_request(&headers, true, true));
            headers.insert("origin", format!("http://{host}").parse().unwrap());
            assert!(same_origin_request(&headers, true, true));
            headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
            assert!(!same_origin_request(&headers, true, true));
        }
        for (host, origin) in [
            ("host.tailnet.ts.net", "https://evil.test"),
            ("localhost:7878", "http://evil.test"),
            ("localhost:7878", "http://localhost:9999"),
            ("localhost:7878", "null"),
            ("localhost:7878", "http://localhost:7878/path"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            headers.insert("origin", origin.parse().unwrap());
            assert!(!same_origin_request(&headers, true, true));
        }
    }

    #[test]
    fn unauthenticated_local_mode_rejects_dns_rebinding() {
        for (host, allowed) in [
            ("localhost:7878", true),
            ("127.0.0.1:7878", true),
            ("[::1]:7878", true),
            ("[::ffff:7f00:1]:7878", true),
            ("[::ffff:127.0.0.1]:7878", false),
            ("evil.test:7878", false),
            ("192.0.2.5:7878", false),
            ("host.tailnet.ts.net", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            headers.insert("origin", format!("http://{host}").parse().unwrap());
            assert_eq!(
                same_origin_request(&headers, true, false),
                allowed,
                "{host}"
            );
        }
    }
}
