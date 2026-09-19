//! Explicit, loopback-only desktop integration. File identity uses the browsing resolver.
use super::*;
use axum::{extract::ConnectInfo, Extension};
use std::net::SocketAddr;

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

fn loopback_request(headers: &HeaderMap, peer: Option<SocketAddr>, mutation: bool) -> bool {
    if !peer.is_some_and(|peer| peer.ip().is_loopback()) {
        return false;
    }
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
    if !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) {
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
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
) -> Result<Json<DesktopCapabilities>, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    Ok(Json(DesktopCapabilities {
        reveal_in_finder: state.desktop_integration
            && loopback_request(
                &headers,
                peer.map(|Extension(ConnectInfo(peer))| peer),
                false,
            ),
    }))
}

pub(crate) async fn reveal(
    State(state): State<Arc<AppState>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RevealFileRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    if !state.desktop_integration
        || !loopback_request(
            &headers,
            peer.map(|Extension(ConnectInfo(peer))| peer),
            true,
        )
    {
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
    fn desktop_requires_loopback_peer_host_and_same_origin() {
        let peer = Some("127.0.0.1:4567".parse().unwrap());
        for host in ["localhost:7878", "127.0.0.1:7878", "[::1]:7878"] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            assert!(loopback_request(&headers, peer, false));
            assert!(!loopback_request(&headers, peer, true));
            headers.insert("origin", format!("http://{host}").parse().unwrap());
            assert!(loopback_request(&headers, peer, true));
            assert!(!loopback_request(&headers, None, true));
            assert!(!loopback_request(
                &headers,
                Some("10.0.0.1:1234".parse().unwrap()),
                true
            ));
            headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
            assert!(!loopback_request(&headers, peer, true));
        }
        for (host, origin) in [
            ("evil.test:7878", "http://evil.test:7878"),
            ("localhost:7878", "http://evil.test"),
            ("localhost:7878", "http://localhost:9999"),
            ("localhost:7878", "null"),
            ("localhost:7878", "http://localhost:7878/path"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("host", host.parse().unwrap());
            headers.insert("origin", origin.parse().unwrap());
            assert!(!loopback_request(&headers, peer, true));
        }
    }
}
