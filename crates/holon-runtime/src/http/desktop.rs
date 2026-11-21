//! Explicit host desktop integration. File identity uses the browsing resolver.
use super::*;
use holon_server::origin::desktop_origin_allowed;

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

pub(crate) async fn capabilities(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<DesktopCapabilities>, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(authentication_error_response)?;
    Ok(Json(DesktopCapabilities {
        reveal_in_finder: state.desktop_integration,
    }))
}

pub(crate) async fn reveal(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RevealFileRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(authentication_error_response)?;
    let authenticated_remote = state.require_control_token
        || state.host.config().auth.mode == crate::authentication::AuthenticationMode::Oidc;
    if !state.desktop_integration || !desktop_origin_allowed(&headers, true, authenticated_remote) {
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

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use holon_server::origin::desktop_origin_allowed;

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
            assert!(desktop_origin_allowed(&headers, false, true));
            assert!(!desktop_origin_allowed(&headers, true, true));
            headers.insert("origin", format!("http://{host}").parse().unwrap());
            assert!(desktop_origin_allowed(&headers, true, true));
            headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
            assert!(!desktop_origin_allowed(&headers, true, true));
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
            assert!(!desktop_origin_allowed(&headers, true, true));
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
                desktop_origin_allowed(&headers, true, false),
                allowed,
                "{host}"
            );
        }
    }
}
