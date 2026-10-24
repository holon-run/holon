use super::*;
use anyhow::Context;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TailscaleState {
    Unavailable,
    Stopped,
    LoggedOut,
    Connected,
    Serving,
    Unknown,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TailscaleServeStatus {
    pub control_authentication_available: bool,
    pub desired_enabled: bool,
    pub state: TailscaleState,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

pub(super) trait Runner {
    fn run(&self, args: &[&str]) -> Result<Value>;
    fn command(&self, args: &[&str]) -> Result<()>;
}

struct Cli;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct DaemonStopped(String);

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
        parse_output(
            args,
            output.status.success(),
            &output.stdout,
            &output.stderr,
        )
    }

    fn command(&self, args: &[&str]) -> Result<()> {
        let output = tailscale_command().args(args).output()?;
        if !output.status.success() {
            return Err(anyhow!("Tailscale Serve operation failed"));
        }
        Ok(())
    }
}

fn parse_output(args: &[&str], success: bool, stdout: &[u8], stderr: &[u8]) -> Result<Value> {
    let parsed = serde_json::from_slice::<Value>(stdout);
    if success {
        return parsed.context("Invalid Tailscale status JSON");
    }
    let backend_status = args == ["status", "--json"];
    // Defensively accept recognized offline JSON despite a failing exit status.
    // Never accept failed Serve inspection or a failed connected-state probe.
    if backend_status
        && parsed.as_ref().is_ok_and(|status| {
            matches!(
                status["BackendState"].as_str(),
                Some("Stopped" | "NoState" | "Starting" | "NeedsLogin" | "NeedsMachineAuth")
            )
        })
    {
        return parsed.map_err(Into::into);
    }
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let diagnostic = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let lower = diagnostic.to_ascii_lowercase();
    if backend_status
        && (lower.contains("tailscale is not running")
            || lower.contains("tailscaled is not running")
            || lower.contains("tailscaled does not appear to be running")
            || lower.contains(
                "failed to connect to local tailscale daemon; it doesn't appear to be running",
            ))
    {
        return Err(DaemonStopped(diagnostic.to_owned()).into());
    }
    Err(anyhow!("Tailscale status probe failed: {diagnostic}"))
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
        state: TailscaleState::Unknown,
        available: true,
        connected: false,
        status_known: false,
        serving: false,
        conflict: false,
        legacy_serving: false,
        hostname: None,
        serve_url: None,
        message: "Could not check Tailscale; retry".into(),
        details: None,
    };
    let status = match runner.run(&["status", "--json"]) {
        Ok(status) => status,
        Err(error) => {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
            {
                result.available = false;
                result.state = TailscaleState::Unavailable;
                result.message =
                    "Tailscale CLI not found (optional); install it or configure its path".into();
            } else if error.downcast_ref::<DaemonStopped>().is_some() {
                result.state = TailscaleState::Stopped;
                result.message = "Start Tailscale to connect".into();
            }
            if result.state == TailscaleState::Unknown {
                result.details = Some(format!("{error:#}"));
            }
            return result;
        }
    };
    result.state = match status["BackendState"].as_str() {
        Some("Running") => TailscaleState::Connected,
        Some("Stopped" | "NoState" | "Starting") => TailscaleState::Stopped,
        Some("NeedsLogin" | "NeedsMachineAuth") => TailscaleState::LoggedOut,
        _ => TailscaleState::Unknown,
    };
    result.connected = result.state == TailscaleState::Connected;
    result.hostname = status["Self"]["DNSName"]
        .as_str()
        .map(|name| name.trim_end_matches('.').to_owned())
        .filter(|name| !name.is_empty());
    if !result.connected {
        result.message = match status["BackendState"].as_str() {
            Some("NeedsLogin") => "Sign in to Tailscale to connect",
            Some("NeedsMachineAuth") => "Approve this device in Tailscale to connect",
            Some("Starting") => "Tailscale is starting; wait for it to connect",
            Some("Stopped" | "NoState") => "Start Tailscale to connect",
            _ => "Could not determine Tailscale state; retry",
        }
        .into();
        return result;
    }
    let Some(hostname) = result.hostname.as_deref() else {
        result.state = TailscaleState::Unknown;
        result.message = "Tailscale hostname unavailable; check MagicDNS".into();
        return result;
    };
    let serve = match runner.run(&["serve", "status", "--json"]) {
        Ok(serve) => serve,
        Err(error) => {
            result.state = TailscaleState::Unknown;
            result.message = "Could not check Tailscale Serve; retry".into();
            result.details = Some(format!("{error:#}"));
            return result;
        }
    };
    if !serve.is_object() || !(serve["Web"].is_null() || serve["Web"].is_object()) {
        result.state = TailscaleState::Unknown;
        result.message = "Could not read Tailscale Serve configuration; retry".into();
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
    if result.serving {
        result.state = TailscaleState::Serving;
    }
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
    match state.loopback_http_addr {
        Some(address) => Ok(format!("http://{address}")),
        None => target_for_addr(&state.host.config().http_addr),
    }
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
    let configured = &state.host.config().http_addr;
    if configured
        .rsplit_once(':')
        .is_some_and(|(host, _)| host.eq_ignore_ascii_case("localhost"))
    {
        return target_for_addr(configured).ok();
    }
    let addr = configured.parse::<std::net::SocketAddr>().ok()?;
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
    authorize_control(&headers, &state).map_err(authentication_error_response)?;
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
    if enabled && state.loopback_http_addr.is_none() {
        return Err(anyhow!(
            "Tailscale Serve requires a bound loopback HTTP listener"
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
    if !before.available || !before.connected || !before.status_known {
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
    authorize_control(&headers, &state).map_err(authentication_error_response)?;
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
    authorize_control(&headers, &state).map_err(authentication_error_response)?;
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

    fn test_state(directory: &std::path::Path, address: &str) -> AppState {
        std::fs::write(
            directory.join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let mut config = AppConfig::load_with_home(Some(directory.to_path_buf())).unwrap();
        config.http_addr = address.into();
        config.control_auth_mode = crate::config::ControlAuthMode::Required;
        config.control_token = Some("test-secret".into());
        let host = RuntimeHost::new_with_provider(
            config,
            Arc::new(crate::provider::StubProvider::new("unused")),
        )
        .unwrap();
        AppState::for_tcp(host)
    }

    #[tokio::test]
    async fn ipv4_wildcard_listener_accepts_loopback_connections() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let state =
            test_state(directory.path(), "0.0.0.0:0").with_http_listener_addresses(address, None);
        assert_eq!(
            target(&state).unwrap(),
            format!("http://127.0.0.1:{}", address.port())
        );
        let client = tokio::net::TcpStream::connect(("127.0.0.1", address.port()))
            .await
            .unwrap();
        let (_, peer) = listener.accept().await.unwrap();
        assert!(peer.ip().is_loopback());
        drop(client);
    }

    #[test]
    fn missing_bound_loopback_is_rejected_before_running_tailscale() {
        struct NoCalls;
        impl Runner for NoCalls {
            fn run(&self, _: &[&str]) -> Result<Value> {
                panic!("invalid listener must be rejected before inspection")
            }
            fn command(&self, _: &[&str]) -> Result<()> {
                panic!("invalid listener must never change Serve rules")
            }
        }
        for address in [
            "127.0.0.1:7878",
            "192.0.2.5:7878",
            "100.64.0.5:7878",
            "[2001:db8::5]:7878",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let mut state = test_state(directory.path(), address);
            if !address.starts_with("127.") {
                state = state.with_http_listener_addresses(address.parse().unwrap(), None);
            }
            let error = change(&state, &NoCalls, true).unwrap_err();
            assert!(error.to_string().contains("bound loopback HTTP listener"));
            assert_eq!(
                load_persisted_config_at(&state.host.config().config_file_path)
                    .unwrap()
                    .tailscale_serve_desired_enabled,
                None
            );
        }
    }

    #[test]
    fn numeric_primary_can_disable_and_reenable_with_bound_loopback() {
        struct MutableServe {
            proxy: std::sync::Mutex<Option<String>>,
            commands: std::sync::Mutex<Vec<Vec<String>>>,
        }
        impl Runner for MutableServe {
            fn run(&self, args: &[&str]) -> Result<Value> {
                if args == ["status", "--json"] {
                    return Ok(json!({
                        "BackendState": "Running",
                        "Self": {"DNSName": "host.example.ts.net."}
                    }));
                }
                let handlers = match self.proxy.lock().unwrap().as_ref() {
                    Some(proxy) => json!({"/": {"Proxy": proxy}}),
                    None => json!({}),
                };
                Ok(json!({"Web": {"host.example.ts.net:443": {"Handlers": handlers}}}))
            }
            fn command(&self, args: &[&str]) -> Result<()> {
                self.commands
                    .lock()
                    .unwrap()
                    .push(args.iter().map(|arg| (*arg).to_owned()).collect());
                *self.proxy.lock().unwrap() = if args.last() == Some(&"off") {
                    None
                } else {
                    args.last().map(|target| (*target).to_owned())
                };
                Ok(())
            }
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let loopback = listener.local_addr().unwrap();
        for primary in ["192.0.2.5:0", "100.64.0.5:0", "[2001:db8::5]:0"] {
            let directory = tempfile::tempdir().unwrap();
            let mut primary_addr: std::net::SocketAddr = primary.parse().unwrap();
            primary_addr.set_port(loopback.port());
            let state = test_state(directory.path(), primary)
                .with_http_listener_addresses(primary_addr, Some(loopback));
            let unix = AppState::for_unix(state.host.clone()).share_listener_state_from(&state);
            let runner = MutableServe {
                proxy: std::sync::Mutex::new(None),
                commands: std::sync::Mutex::new(Vec::new()),
            };
            let expected_target = format!("http://{loopback}");
            assert_eq!(target(&state).unwrap(), expected_target);
            assert_eq!(target(&unix).unwrap(), expected_target);
            assert!(change(&state, &runner, true).unwrap().serving);
            let disabled = change(&unix, &runner, false).unwrap();
            assert!(!disabled.serving);
            assert!(!disabled.desired_enabled);
            let enabled = change(&unix, &runner, true).unwrap();
            assert!(enabled.serving);
            assert!(enabled.desired_enabled);
            assert_eq!(
                runner.commands.lock().unwrap().as_slice(),
                [
                    vec![
                        "serve",
                        "--bg",
                        "--https=443",
                        "--set-path=/",
                        &expected_target
                    ],
                    vec!["serve", "--https=443", "--set-path=/", "off"],
                    vec![
                        "serve",
                        "--bg",
                        "--https=443",
                        "--set-path=/",
                        &expected_target
                    ],
                ]
            );
            assert_eq!(
                load_persisted_config_at(&state.host.config().config_file_path)
                    .unwrap()
                    .tailscale_serve_desired_enabled,
                Some(true)
            );
        }
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
            let state = AppState::for_tcp(host)
                .with_http_listener_addresses("127.0.0.1:7878".parse().unwrap(), None);
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
            } else {
                assert!(change(&state, &runner, true)
                    .unwrap_err()
                    .to_string()
                    .contains("requires Holon control authentication"));
            }
        }
    }

    #[test]
    fn localhost_legacy_rule_can_migrate_to_resolved_listener() {
        let directory = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
        let bound = listener.local_addr().unwrap();
        let configured = format!("localhost:{}", bound.port());
        let state =
            test_state(directory.path(), &configured).with_http_listener_addresses(bound, None);
        let legacy = format!("http://{configured}");
        let runner = Mock {
            serve: json!({"Web": {"host.example.ts.net:443": {
                "Handlers": {"/": {"Proxy": legacy}}
            }}}),
        };
        let status = read(&state, &runner).unwrap();
        assert!(status.serving);
        assert!(status.legacy_serving);
        assert!(!status.conflict);
        assert_eq!(target(&state).unwrap(), format!("http://{bound}"));
    }

    #[tokio::test]
    async fn ipv6_wildcard_listener_accepts_selected_backend_target() {
        let listener = tokio::net::TcpListener::bind("[::]:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let state =
            test_state(directory.path(), "[::]:0").with_http_listener_addresses(address, None);
        assert_eq!(
            target(&state).unwrap(),
            format!("http://[::1]:{}", address.port())
        );
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

    struct Backend(Value);

    impl Runner for Backend {
        fn run(&self, args: &[&str]) -> Result<Value> {
            assert_eq!(args, ["status", "--json"]);
            Ok(self.0.clone())
        }

        fn command(&self, _: &[&str]) -> Result<()> {
            panic!("backend inspection must not change Serve")
        }
    }

    struct FailedProbe(std::io::ErrorKind);

    impl Runner for FailedProbe {
        fn run(&self, _: &[&str]) -> Result<Value> {
            Err(std::io::Error::new(self.0, "probe diagnostic").into())
        }

        fn command(&self, _: &[&str]) -> Result<()> {
            panic!("a failed probe must not change Serve")
        }
    }

    #[test]
    fn only_a_missing_cli_is_unavailable() {
        for (kind, expected) in [
            (std::io::ErrorKind::NotFound, TailscaleState::Unavailable),
            (
                std::io::ErrorKind::PermissionDenied,
                TailscaleState::Unknown,
            ),
            (std::io::ErrorKind::TimedOut, TailscaleState::Unknown),
            (std::io::ErrorKind::InvalidInput, TailscaleState::Unknown),
            (std::io::ErrorKind::Other, TailscaleState::Unknown),
        ] {
            let status = inspect(&FailedProbe(kind), true, "http://127.0.0.1:7878", None);
            assert_eq!(status.state, expected);
            assert_eq!(status.available, expected != TailscaleState::Unavailable);
            assert!(status.desired_enabled);
            assert!(!status.connected);
            assert!(!status.status_known);
            assert!(!status.serving);
            if expected == TailscaleState::Unknown {
                assert!(status
                    .details
                    .as_deref()
                    .unwrap()
                    .contains("probe diagnostic"));
            } else {
                assert!(status.details.is_none());
            }
            assert!(!status.message.contains("probe diagnostic"));
            if expected == TailscaleState::Unavailable {
                assert!(status.message.contains("CLI not found (optional)"));
                assert!(!status.message.contains("not installed"));
                assert!(serde_json::to_value(status)
                    .unwrap()
                    .get("details")
                    .is_none());
            }
        }
    }

    #[test]
    fn disconnected_backend_states_are_installed_but_not_mutable() {
        for (backend, expected) in [
            ("Stopped", TailscaleState::Stopped),
            ("NoState", TailscaleState::Stopped),
            ("Starting", TailscaleState::Stopped),
            ("NeedsLogin", TailscaleState::LoggedOut),
            ("NeedsMachineAuth", TailscaleState::LoggedOut),
            ("FutureBackendState", TailscaleState::Unknown),
        ] {
            let status = inspect(
                &Backend(json!({"BackendState": backend})),
                false,
                "http://127.0.0.1:7878",
                None,
            );
            assert_eq!(status.state, expected);
            assert!(status.available);
            assert!(!status.connected);
            assert!(!status.status_known);
            assert!(!status.serving);
            assert!(status.details.is_none());
        }
        for malformed in [
            json!(null),
            json!([]),
            json!({}),
            json!({"BackendState": 1}),
        ] {
            let status = inspect(&Backend(malformed), false, "http://127.0.0.1:7878", None);
            assert_eq!(status.state, TailscaleState::Unknown);
            assert!(status.available);
            assert!(!status.status_known);
        }
    }

    #[test]
    fn backend_json_preserves_expected_disconnected_states() {
        for success in [true, false] {
            for (backend, expected) in [
                ("Stopped", TailscaleState::Stopped),
                ("NoState", TailscaleState::Stopped),
                ("Starting", TailscaleState::Stopped),
                ("NeedsLogin", TailscaleState::LoggedOut),
                ("NeedsMachineAuth", TailscaleState::LoggedOut),
            ] {
                let stdout = serde_json::to_vec(&json!({"BackendState": backend})).unwrap();
                let parsed = parse_output(&["status", "--json"], success, &stdout, b"").unwrap();
                let status = inspect(&Backend(parsed), false, "http://127.0.0.1:7878", None);
                assert_eq!(status.state, expected);
                assert!(status.available);
                assert!(!status.connected);
                assert!(!status.status_known);
                assert!(status.details.is_none());
                assert!(
                    parse_output(&["serve", "status", "--json"], false, &stdout, b"denied")
                        .is_err()
                );
            }
        }
        assert!(parse_output(
            &["status", "--json"],
            false,
            br#"{"BackendState":"Running"}"#,
            b"unexpected probe error"
        )
        .is_err());
        assert!(parse_output(&["serve", "status", "--json"], false, b"{}", b"denied").is_err());
    }

    #[test]
    fn daemon_stop_diagnostics_are_not_a_missing_installation() {
        struct OutputProbe(&'static [u8]);
        impl Runner for OutputProbe {
            fn run(&self, args: &[&str]) -> Result<Value> {
                parse_output(args, false, b"", self.0)
            }

            fn command(&self, _: &[&str]) -> Result<()> {
                panic!("daemon stop inspection must not change Serve")
            }
        }
        for (diagnostic, expected) in [
            (&b"Tailscale is not running."[..], TailscaleState::Stopped),
            (
                &b"failed to connect to local Tailscale daemon; it doesn't appear to be running"[..],
                TailscaleState::Stopped,
            ),
            (&b"permission denied"[..], TailscaleState::Unknown),
            (
                &b"failed to connect to local Tailscale daemon (which appears to be running)"[..],
                TailscaleState::Unknown,
            ),
        ] {
            let status = inspect(
                &OutputProbe(diagnostic),
                false,
                "http://127.0.0.1:7878",
                None,
            );
            assert_eq!(status.state, expected);
            assert!(status.available);
            assert!(!status.status_known);
            assert_eq!(
                status.details.is_some(),
                expected == TailscaleState::Unknown
            );
        }
    }

    #[test]
    fn invalid_probe_output_is_unknown_not_a_missing_cli() {
        struct OutputProbe {
            success: bool,
            stdout: &'static [u8],
        }
        impl Runner for OutputProbe {
            fn run(&self, args: &[&str]) -> Result<Value> {
                parse_output(args, self.success, self.stdout, b"probe diagnostic")
            }

            fn command(&self, _: &[&str]) -> Result<()> {
                panic!("invalid probe output must not change Serve")
            }
        }
        for success in [false, true] {
            for stdout in [
                &b"not JSON"[..],
                br#"{"BackendState":"NeedsLogin"} trailing garbage"#,
                b"null",
                b"[]",
                b"{}",
                br#"{"BackendState":1}"#,
                br#"{"BackendState":"FutureBackendState"}"#,
            ] {
                let status = inspect(
                    &OutputProbe { success, stdout },
                    false,
                    "http://127.0.0.1:7878",
                    None,
                );
                assert_eq!(status.state, TailscaleState::Unknown);
                assert!(status.available);
                assert!(!status.connected);
                assert!(!status.status_known);
                assert!(!status.serving);
            }
        }
        assert!(parse_output(
            &["serve", "status", "--json"],
            false,
            br#"{"BackendState":"NeedsLogin"}"#,
            b"probe diagnostic"
        )
        .is_err());
    }

    #[test]
    fn malformed_json_is_a_probe_error() {
        let error = parse_output(&["status", "--json"], true, b"not JSON", b"").unwrap_err();
        assert!(format!("{error:#}").contains("Invalid Tailscale status JSON"));
    }

    #[test]
    fn connected_backend_without_hostname_is_unknown() {
        let status = inspect(
            &Backend(json!({"BackendState": "Running"})),
            false,
            "http://127.0.0.1:7878",
            None,
        );
        assert_eq!(status.state, TailscaleState::Unknown);
        assert!(status.available);
        assert!(status.connected);
        assert!(!status.status_known);
    }

    #[test]
    fn failed_serve_probe_does_not_report_a_missing_cli_or_known_status() {
        struct FailedServe;
        impl Runner for FailedServe {
            fn run(&self, args: &[&str]) -> Result<Value> {
                if args == ["status", "--json"] {
                    return Mock { serve: json!({}) }.run(args);
                }
                parse_output(args, false, b"{}", b"Serve access denied")
            }

            fn command(&self, _: &[&str]) -> Result<()> {
                panic!("unknown Serve status must not be mutated")
            }
        }
        let status = inspect(&FailedServe, true, "http://127.0.0.1:7878", None);
        assert_eq!(status.state, TailscaleState::Unknown);
        assert!(status.available);
        assert!(status.connected);
        assert!(!status.status_known);
        assert!(!status.serving);
        assert!(status
            .details
            .as_deref()
            .unwrap()
            .contains("Serve access denied"));
    }

    #[test]
    fn json_state_is_additive_and_details_are_optional() {
        for (state, name) in [
            (TailscaleState::Unavailable, "unavailable"),
            (TailscaleState::Stopped, "stopped"),
            (TailscaleState::LoggedOut, "logged_out"),
            (TailscaleState::Connected, "connected"),
            (TailscaleState::Serving, "serving"),
            (TailscaleState::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
        let status = inspect(
            &Mock { serve: json!({}) },
            false,
            "http://127.0.0.1:7878",
            None,
        );
        assert_eq!(status.state, TailscaleState::Connected);
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["state"], "connected");
        assert_eq!(json["available"], true);
        assert_eq!(json["connected"], true);
        assert_eq!(json["status_known"], true);
        assert_eq!(json["serving"], false);
        assert!(json.get("details").is_none());
    }

    #[test]
    fn unknown_backend_cannot_enable_or_change_desired_state() {
        let directory = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state = test_state(directory.path(), &address.to_string())
            .with_http_listener_addresses(address, None);
        for backend in ["Stopped", "NeedsLogin", "FutureBackendState"] {
            assert!(change(&state, &Backend(json!({"BackendState": backend})), true).is_err());
        }
        assert!(change(
            &state,
            &FailedProbe(std::io::ErrorKind::PermissionDenied),
            true
        )
        .is_err());
        assert_eq!(
            load_persisted_config_at(&state.host.config().config_file_path)
                .unwrap()
                .tailscale_serve_desired_enabled,
            None
        );
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
        assert_eq!(status.state, TailscaleState::Serving);
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
