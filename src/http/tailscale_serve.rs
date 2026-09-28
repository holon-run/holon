use super::*;
use std::process::Command;

#[derive(Debug, Serialize, JsonSchema)]
pub struct TailscaleServeStatus {
    pub desired_enabled: bool,
    pub available: bool,
    pub connected: bool,
    pub status_known: bool,
    pub serving: bool,
    pub conflict: bool,
    pub hostname: Option<String>,
    pub serve_url: Option<String>,
    pub message: String,
}

pub(super) trait Runner {
    fn run(&self, args: &[&str]) -> Result<Value>;
    fn command(&self, args: &[&str]) -> Result<()>;
}

struct Cli;

fn tailscale_command() -> Command {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        let candidate = std::env::var("TAILSCALE_BINARY_PATH")
            .ok()
            .into_iter()
            .chain([
                "/usr/local/bin/tailscale".into(),
                "/opt/homebrew/bin/tailscale".into(),
                "/usr/bin/tailscale".into(),
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale".into(),
            ])
            .find(|path| {
                std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            });
        if let Some(candidate) = candidate {
            return Command::new(candidate);
        }
    }
    Command::new("tailscale")
}

impl Runner for Cli {
    fn run(&self, args: &[&str]) -> Result<Value> {
        let output = tailscale_command().args(args).output()?;
        if !output.status.success() {
            return Err(anyhow!("Tailscale status unavailable"));
        }
        serde_json::from_slice(&output.stdout).map_err(Into::into)
    }

    fn command(&self, args: &[&str]) -> Result<()> {
        let output = tailscale_command().args(args).output()?;
        if !output.status.success() {
            return Err(anyhow!("Tailscale Serve operation failed"));
        }
        Ok(())
    }
}

fn inspect(
    runner: &impl Runner,
    desired_enabled: bool,
    target: &str,
    legacy_target: Option<&str>,
) -> TailscaleServeStatus {
    let mut result = TailscaleServeStatus {
        desired_enabled,
        available: false,
        connected: false,
        status_known: false,
        serving: false,
        conflict: false,
        hostname: None,
        serve_url: None,
        message: "Tailscale unavailable".into(),
    };
    let Ok(status) = runner.run(&["status", "--json"]) else {
        return result;
    };
    result.available = true;
    result.connected = status["BackendState"] == "Running";
    result.hostname = status["Self"]["DNSName"]
        .as_str()
        .map(|name| name.trim_end_matches('.').to_owned())
        .filter(|name| !name.is_empty());
    if !result.connected {
        result.message = "Tailscale is not connected".into();
        return result;
    }
    let Some(hostname) = result.hostname.as_deref() else {
        result.message = "Tailscale hostname unavailable".into();
        return result;
    };
    let Ok(serve) = runner.run(&["serve", "status", "--json"]) else {
        result.message = "Tailscale Serve status unavailable".into();
        return result;
    };
    if !serve.is_object() || !(serve["Web"].is_null() || serve["Web"].is_object()) {
        result.message = "Tailscale Serve configuration unavailable".into();
        return result;
    }
    let site = serve["Web"]
        .as_object()
        .and_then(|web| web.get(&format!("{hostname}:443")));
    let root = site.and_then(|site| site["Handlers"]["/"].as_object());
    result.serving = root.is_some_and(|root| {
        root.get("Proxy").and_then(Value::as_str) == Some(target)
            || legacy_target
                .is_some_and(|legacy| root.get("Proxy").and_then(Value::as_str) == Some(legacy))
    });
    if result.serving {
        result.serve_url = Some(format!("https://{hostname}"));
    }
    result.conflict = (root.is_some() && !result.serving)
        || serve["TCP"]
            .as_object()
            .is_some_and(|tcp| tcp.contains_key("443"));
    result.status_known = true;
    result.message = if result.conflict {
        "A different Tailscale Serve root rule exists"
    } else if result.serving {
        "Holon is served"
    } else if desired_enabled {
        "Desired sharing is enabled, but the Serve root rule is absent"
    } else {
        "Sharing is disabled"
    }
    .into();
    result
}

fn target(state: &AppState) -> String {
    let addr = &state.host.config().http_addr;
    if let Ok(socket) = addr.parse::<std::net::SocketAddr>() {
        let ip = if socket.ip().is_unspecified() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            socket.ip()
        };
        return match ip {
            std::net::IpAddr::V4(_) => format!("http://{ip}:{}", socket.port()),
            std::net::IpAddr::V6(_) => format!("http://[{ip}]:{}", socket.port()),
        };
    }
    format!("http://{addr}")
}

fn legacy_target(state: &AppState) -> Option<String> {
    let addr = state
        .host
        .config()
        .http_addr
        .parse::<std::net::SocketAddr>()
        .ok()?;
    if addr.ip().is_unspecified() || addr.ip().is_loopback() {
        return None;
    }
    Some(format!("http://{addr}"))
}

fn read(state: &AppState, runner: &impl Runner) -> Result<TailscaleServeStatus> {
    let config = state.host.config();
    let stored = load_persisted_config_at(&config.config_file_path)?;
    Ok(inspect(
        runner,
        stored.tailscale_serve_desired_enabled.unwrap_or(false),
        &target(state),
        legacy_target(state).as_deref(),
    ))
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let status = tokio::task::spawn_blocking(move || read(&state, &Cli))
        .await
        .map_err(|err| error_response(err.into()))?
        .map_err(error_response)?;
    Ok(Json(status))
}

pub(super) fn change(
    state: &AppState,
    runner: &impl Runner,
    enabled: bool,
) -> Result<TailscaleServeStatus> {
    let _guard = state
        .tailscale_serve_change
        .lock()
        .map_err(|_| anyhow!("Tailscale Serve change lock unavailable"))?;
    let config = state.host.config();
    let mut stored = load_persisted_config_at(&config.config_file_path)?;
    let before = inspect(
        runner,
        stored.tailscale_serve_desired_enabled.unwrap_or(false),
        &target(state),
        legacy_target(state).as_deref(),
    );
    if !before.status_known {
        return Err(anyhow!("Tailscale Serve is unavailable"));
    }
    if enabled {
        if before.conflict {
            return Err(anyhow!("A different Tailscale Serve root rule exists"));
        }
        if !before.serving {
            runner.command(&[
                "serve",
                "--bg",
                "--https=443",
                "--set-path=/",
                &target(state),
            ])?;
        }
    } else if before.conflict {
        return Err(anyhow!("Cannot remove a conflicting Serve rule"));
    } else if before.serving {
        runner.command(&["serve", "--https=443", "--set-path=/", "off"])?;
    }
    stored.tailscale_serve_desired_enabled = Some(enabled);
    save_persisted_config_at(&config.config_file_path, &stored)?;
    read(state, runner)
}

pub async fn enable(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let status = tokio::task::spawn_blocking(move || change(&state, &Cli, true))
        .await
        .map_err(|err| error_response(err.into()))?
        .map_err(error_response)?;
    Ok(Json(status))
}

pub async fn disable(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    let status = tokio::task::spawn_blocking(move || change(&state, &Cli, false))
        .await
        .map_err(|err| error_response(err.into()))?
        .map_err(error_response)?;
    Ok(Json(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock {
        serve: Value,
    }

    impl Runner for Mock {
        fn run(&self, args: &[&str]) -> Result<Value> {
            if args == ["status", "--json"] {
                return Ok(serde_json::json!({
                    "BackendState": "Running",
                    "Self": {"DNSName": "host.example.ts.net."}
                }));
            }
            Ok(self.serve.clone())
        }

        fn command(&self, _: &[&str]) -> Result<()> {
            panic!("inspection must not execute Serve commands")
        }
    }

    #[test]
    fn desired_state_survives_external_rule_removal() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({"Web": {}}),
            },
            true,
            "http://127.0.0.1:7878",
            None,
        );
        assert!(status.desired_enabled);
        assert!(!status.serving);
        assert!(!status.conflict);
    }

    #[test]
    fn legacy_listener_is_owned_and_other_paths_are_preserved() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({"Web": {
                    "host.example.ts.net:443": {"Handlers": {
                        "/": {"Proxy": "http://192.0.2.5:7878"},
                        "/other": {"Proxy": "http://127.0.0.1:9000"}
                    }}
                }}),
            },
            true,
            "http://127.0.0.1:7878",
            Some("http://192.0.2.5:7878"),
        );
        assert!(status.serving);
        assert!(!status.conflict);
        assert!(status.status_known);
    }

    #[test]
    fn only_current_hostname_root_is_a_conflict() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({"Web": {
                    "host.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9000"}}},
                    "other.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:7878"}}}
                }}),
            },
            true,
            "http://127.0.0.1:7878",
            None,
        );
        assert!(!status.serving);
        assert!(status.conflict);
    }

    #[test]
    fn unrelated_site_does_not_appear_as_owned_root() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({"Web": {
                    "other.example.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:7878"}}}
                }}),
            },
            false,
            "http://127.0.0.1:7878",
            None,
        );
        assert!(!status.serving);
        assert!(!status.conflict);
    }
}
