use super::*;
use std::process::Command;

#[derive(Debug, Serialize, JsonSchema)]
pub struct TailscaleServeStatus {
    pub control_authentication_available: bool,
    pub desired_enabled: bool,
    pub available: bool,
    pub connected: bool,
    pub status_known: bool,
    pub serving: bool,
    pub conflict: bool,
    #[serde(skip)]
    #[schemars(skip)]
    legacy_serving: bool,
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
        control_authentication_available: false,
        desired_enabled,
        available: false,
        connected: false,
        status_known: false,
        serving: false,
        conflict: false,
        legacy_serving: false,
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
    let proxy = root
        .and_then(|root| root.get("Proxy"))
        .and_then(Value::as_str);
    result.legacy_serving = legacy_target.is_some_and(|legacy| proxy == Some(legacy));
    result.serving = proxy == Some(target) || result.legacy_serving;
    if result.serving {
        result.serve_url = Some(format!("https://{hostname}"));
    }
    result.conflict = (root.is_some() && !result.serving && !result.legacy_serving)
        || serve["TCP"]
            .as_object()
            .and_then(|tcp| tcp.get("443"))
            .is_some_and(|listener| listener["HTTPS"] != true);
    result.status_known = true;
    result.message = if result.conflict {
        "A different Tailscale Serve root rule exists"
    } else if result.legacy_serving {
        "Holon uses a legacy LAN Serve target; enable to migrate to loopback"
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

fn target_for_addr(addr: &str) -> Result<String> {
    if let Ok(socket) = addr.parse::<std::net::SocketAddr>() {
        return Ok(match socket {
            std::net::SocketAddr::V4(_) => format!("http://127.0.0.1:{}", socket.port()),
            std::net::SocketAddr::V6(socket)
                if socket.ip().is_loopback() || socket.ip().is_unspecified() =>
            {
                format!("http://[::1]:{}", socket.port())
            }
            std::net::SocketAddr::V6(_) => format!("http://127.0.0.1:{}", socket.port()),
        });
    }
    let (host, port) = addr
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("HTTP listener address must include a port"))?;
    let port = port.parse::<u16>()?;
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(format!("http://localhost:{port}"));
    }
    Ok(format!("http://127.0.0.1:{port}"))
}

fn target(state: &AppState) -> Result<String> {
    target_for_addr(&state.host.config().http_addr)
}

fn listener_includes_loopback(addr: &str) -> bool {
    // Serve enable validates the configured primary listener, not CLI-added sockets.
    addr.parse::<std::net::SocketAddr>()
        .map(|socket| socket.ip().is_loopback() || socket.ip().is_unspecified())
        .unwrap_or_else(|_| {
            addr.rsplit_once(':').is_some_and(|(host, port)| {
                host.eq_ignore_ascii_case("localhost") && port.parse::<u16>().is_ok()
            })
        })
}

pub(super) fn serve_authentication_available(state: &AppState) -> bool {
    let config = state.host.config();
    authenticated_control_available(
        config.auth.mode,
        config.control_token_required(ControlTransportKind::Tcp),
        config.control_token.as_deref(),
    )
}

fn authenticated_control_available(
    auth_mode: crate::authentication::AuthenticationMode,
    require_control_token: bool,
    control_token: Option<&str>,
) -> bool {
    auth_mode == crate::authentication::AuthenticationMode::Oidc
        || (require_control_token && control_token.is_some_and(|token| !token.trim().is_empty()))
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
    let target = target(state)?;
    let mut status = inspect(
        runner,
        stored.tailscale_serve_desired_enabled.unwrap_or(false),
        &target,
        legacy_target(state).as_deref(),
    );
    status.control_authentication_available = serve_authentication_available(state);
    Ok(status)
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
    if enabled && !listener_includes_loopback(&config.http_addr) {
        return Err(anyhow!(
            "Tailscale Serve requires a loopback or wildcard primary listener; use --listen 0.0.0.0:PORT or a loopback address"
        ));
    }
    let mut stored = load_persisted_config_at(&config.config_file_path)?;
    let target = target(state)?;
    let before = inspect(
        runner,
        stored.tailscale_serve_desired_enabled.unwrap_or(false),
        &target,
        legacy_target(state).as_deref(),
    );
    if !before.status_known {
        return Err(anyhow!("Tailscale Serve is unavailable"));
    }
    if enabled {
        if !serve_authentication_available(state) {
            return Err(anyhow!(
                "Tailscale Serve requires Holon control authentication (configure a control token or OIDC)"
            ));
        }
        if before.conflict {
            return Err(anyhow!("A different Tailscale Serve root rule exists"));
        }
        if !before.serving || before.legacy_serving {
            runner.command(&["serve", "--bg", "--https=443", "--set-path=/", &target])?;
        }
    } else if before.conflict {
        return Err(anyhow!("Cannot remove a conflicting Serve rule"));
    } else if before.serving || before.legacy_serving {
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
    use crate::config::AppConfig;

    #[tokio::test]
    async fn ipv4_wildcard_listener_accepts_loopback_connections() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        assert!(listener_includes_loopback(&address.to_string()));
        let client = tokio::net::TcpStream::connect(("127.0.0.1", address.port()))
            .await
            .unwrap();
        let (_, peer) = listener.accept().await.unwrap();
        assert!(peer.ip().is_loopback());
        drop(client);
        for address in ["192.0.2.5:7878", "100.64.0.5:7878", "[2001:db8::5]:7878"] {
            assert!(!listener_includes_loopback(address));
        }
    }

    #[test]
    fn numeric_lan_primary_enable_is_rejected_before_running_tailscale() {
        struct NoCalls;
        impl Runner for NoCalls {
            fn run(&self, _: &[&str]) -> Result<Value> {
                panic!("invalid listener must be rejected before inspection")
            }
            fn command(&self, _: &[&str]) -> Result<()> {
                panic!("invalid listener must never change Serve rules")
            }
        }
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut config = AppConfig::load_with_home(Some(directory.path().to_path_buf())).unwrap();
        config.http_addr = "192.0.2.5:7878".into();
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("unused")),
        )
        .unwrap();
        let state = AppState::for_tcp(host);
        let error = change(&state, &NoCalls, true).unwrap_err();
        assert!(error
            .to_string()
            .contains("loopback or wildcard primary listener"));
    }

    #[test]
    fn serve_target_uses_loopback_even_with_lan_listener() {
        assert_eq!(
            target_for_addr("192.0.2.5:7878").unwrap(),
            "http://127.0.0.1:7878"
        );
        assert_eq!(
            target_for_addr("[2001:db8::5]:7878").unwrap(),
            "http://127.0.0.1:7878"
        );
        assert_eq!(target_for_addr("[::1]:7878").unwrap(), "http://[::1]:7878");
        assert_eq!(
            target_for_addr("localhost:7878").unwrap(),
            "http://localhost:7878"
        );
    }

    #[test]
    fn enable_requires_effective_control_authentication() {
        use crate::authentication::AuthenticationMode::{Local, Oidc};
        assert!(!authenticated_control_available(Local, false, None));
        assert!(!authenticated_control_available(
            Local,
            false,
            Some("secret")
        ));
        assert!(!authenticated_control_available(Local, true, Some(" ")));
        assert!(authenticated_control_available(Local, true, Some("secret")));
        assert!(authenticated_control_available(Oidc, false, None));
    }

    #[test]
    fn status_reports_effective_authentication_not_token_presence() {
        struct ControlMock(Mock);
        impl Runner for ControlMock {
            fn run(&self, args: &[&str]) -> Result<Value> {
                self.0.run(args)
            }

            fn command(&self, _: &[&str]) -> Result<()> {
                Ok(())
            }
        }
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config = AppConfig::load_with_home(Some(directory.path().to_path_buf())).unwrap();
        let runner = ControlMock(Mock { serve: json!({}) });
        for mode in [
            crate::config::ControlAuthMode::Disabled,
            crate::config::ControlAuthMode::Required,
        ] {
            let mut config = config.clone();
            config.control_auth_mode = mode;
            config.control_token = Some("secret".into());
            let host = RuntimeHost::new_with_provider(
                config,
                Arc::new(crate::provider::StubProvider::new("unused")),
            )
            .unwrap();
            let state = AppState::for_tcp(host);
            let status = read(&state, &runner).unwrap();
            assert_eq!(
                status.control_authentication_available,
                mode == crate::config::ControlAuthMode::Required
            );
            assert_eq!(
                change(&state, &runner, false)
                    .unwrap()
                    .control_authentication_available,
                mode == crate::config::ControlAuthMode::Required
            );
            if mode == crate::config::ControlAuthMode::Required {
                assert!(
                    change(&state, &runner, true)
                        .unwrap()
                        .control_authentication_available
                );
            }
        }
    }

    #[tokio::test]
    async fn ipv6_wildcard_listener_accepts_selected_backend_target() {
        let listener = tokio::net::TcpListener::bind("[::]:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        assert!(listener_includes_loopback(&address.to_string()));
        let target = target_for_addr(&address.to_string()).unwrap();
        assert_eq!(target, format!("http://[::1]:{}", address.port()));
        let client = tokio::net::TcpStream::connect(("::1", address.port()))
            .await
            .unwrap();
        let (_, peer) = listener.accept().await.unwrap();
        assert!(peer.ip().is_loopback());
        drop(client);
    }

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
        assert!(status.legacy_serving);
        assert!(!status.conflict);
        assert!(status.status_known);
        assert_eq!(
            status.serve_url.as_deref(),
            Some("https://host.example.ts.net")
        );
        assert!(status.message.contains("enable to migrate"));
    }

    #[test]
    fn legacy_listener_remains_reported_as_serving_when_desired_is_disabled() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({"Web": {
                    "host.example.ts.net:443": {"Handlers": {
                        "/": {"Proxy": "http://192.0.2.5:7878"}
                    }}
                }}),
            },
            false,
            "http://127.0.0.1:7878",
            Some("http://192.0.2.5:7878"),
        );
        assert!(!status.desired_enabled);
        assert!(status.serving);
        assert!(!status.conflict);
        assert_eq!(
            status.serve_url.as_deref(),
            Some("https://host.example.ts.net")
        );
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

    #[test]
    fn https_listener_for_owned_root_is_not_a_conflict() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({
                    "TCP": {"443": {"HTTPS": true}},
                    "Web": {"host.example.ts.net:443": {"Handlers": {
                        "/": {"Proxy": "http://127.0.0.1:7878"}
                    }}}
                }),
            },
            true,
            "http://127.0.0.1:7878",
            None,
        );
        assert!(status.serving);
        assert!(!status.conflict);
        assert_eq!(status.message, "Holon is served");
    }

    #[test]
    fn tcp_forward_listener_is_a_conflict() {
        let status = inspect(
            &Mock {
                serve: serde_json::json!({"TCP": {"443": {
                    "TCPForward": "127.0.0.1:9000"
                }}, "Web": {}}),
            },
            true,
            "http://127.0.0.1:7878",
            None,
        );
        assert!(!status.serving);
        assert!(status.conflict);
    }
}
