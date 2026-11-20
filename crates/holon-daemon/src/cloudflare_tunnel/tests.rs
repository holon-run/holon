use super::*;

const PUBLIC_URL: &str = "https://holon.example.com";
const SECRET: &str = "never-publish-this-token";

#[test]
fn production_command_uses_json_logs_and_ignores_ambient_config() {
    let command = tunnel_command(Path::new("/connector.token"));
    let args = command.as_std().get_args().collect::<Vec<_>>();
    assert_eq!(
        args,
        [
            "tunnel",
            "--config",
            "/dev/null",
            "--no-autoupdate",
            "--output",
            "json",
            "run",
            "--token-file",
            "/connector.token",
        ]
    );
}

#[test]
fn failed_status_handle_retains_fixed_safe_status() {
    let handle = TunnelStatusHandle::failed(PUBLIC_URL);
    let expected = TunnelStatus {
        phase: TunnelPhase::Failed,
        public_url: PUBLIC_URL.to_owned(),
        message: "Could not start cloudflared; local service remains available".to_owned(),
    };
    assert_eq!(handle.snapshot(), expected);
    assert_eq!(handle.clone().snapshot(), expected);
}

fn fake_command(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[tokio::test]
async fn child_environment_is_limited_to_explicit_allowlist() {
    let secret_names = [
        "HOLON_CONTROL_TOKEN",
        "HOLON_CALLER_AGENT_ID",
        "HOLON_CALLER_CONTEXT_FILE",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "DEEPSEEK_API_KEY",
        "XAI_API_KEY",
        "TUNNEL_TOKEN",
        "TUNNEL_TOKEN_FILE",
        "TUNNEL_CRED_CONTENTS",
        "TUNNEL_CRED_FILE",
        "UNRELATED_SECRET",
    ];
    let script = secret_names
        .iter()
        .map(|name| format!("test -z \"${{{name}+x}}\" || exit 1"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut command = fake_command(&script);
    for name in secret_names {
        command.env(name, SECRET);
    }
    command.env("PATH", "/must-not-override-parent-path");
    configure_child(&mut command);
    for (name, value) in command.as_std().get_envs() {
        assert!(CHILD_ENV_ALLOWLIST.iter().any(|allowed| name == *allowed));
        assert!(value == std::env::var_os(name).as_deref());
    }
    for name in CHILD_ENV_ALLOWLIST {
        if std::env::var_os(name).is_some() {
            assert!(command
                .as_std()
                .get_envs()
                .any(|(actual, value)| actual == *name && value.is_some()));
        }
    }
    assert!(command.status().await.unwrap().success());
}

fn status_channel() -> (watch::Sender<TunnelStatus>, TunnelStatusHandle) {
    let (sender, receiver) = watch::channel(TunnelStatus {
        phase: TunnelPhase::Starting,
        public_url: PUBLIC_URL.to_owned(),
        message: "Waiting for cloudflared".to_owned(),
    });
    (sender, TunnelStatusHandle { receiver })
}

async fn wait_for_phase(handle: &TunnelStatusHandle, phase: TunnelPhase) -> TunnelStatus {
    let mut receiver = handle.receiver.clone();
    let status = tokio::time::timeout(
        Duration::from_secs(3),
        receiver.wait_for(|status| status.phase == phase),
    )
    .await
    .expect("status transition timed out")
    .expect("supervisor stopped before status transition")
    .clone();
    status
}

fn pid_script(path: &Path, suffix: &str) -> String {
    // tempfile paths contain no single quotes.
    format!("printf '%s' $$ > '{}'; {suffix}", path.display())
}

fn assert_reaped(path: &Path) {
    let pid: libc::pid_t = std::fs::read_to_string(path).unwrap().parse().unwrap();
    let mut exit_status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut exit_status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[tokio::test]
async fn connects_and_shutdown_kills_and_reaps() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pid");
    let script = pid_script(
        &pid_file,
        r#"printf '%s\n' '{"message":"Registered tunnel connection","connIndex":0}' >&2; exec sleep 30"#,
    );
    let tunnel = CloudflareTunnel::start_with_command(
        fake_command(&script),
        PUBLIC_URL,
        Duration::from_secs(1),
    )
    .unwrap();
    let handle = tunnel.status_handle();
    let connected = wait_for_phase(&handle, TunnelPhase::Connected).await;
    assert_eq!(connected.public_url, PUBLIC_URL);
    assert!(connected.message.contains("not verified"));
    tunnel.shutdown().await.unwrap();
    assert_eq!(handle.snapshot().phase, TunnelPhase::Stopped);
    assert_reaped(&pid_file);
}

#[tokio::test]
async fn startup_timeout_does_not_terminate_the_child() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pid");
    let tunnel = CloudflareTunnel::start_with_command(
        fake_command(&pid_script(&pid_file, "exec sleep 30")),
        PUBLIC_URL,
        Duration::from_millis(100),
    )
    .unwrap();
    let handle = tunnel.status_handle();
    let failed = wait_for_phase(&handle, TunnelPhase::Failed).await;
    assert!(failed.message.contains("startup timeout"));
    let pid: libc::pid_t = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    tunnel.shutdown().await.unwrap();
    assert_reaped(&pid_file);
}

#[tokio::test]
async fn nonzero_exit_does_not_expose_secret_logs() {
    let tunnel = CloudflareTunnel::start_with_command(
        fake_command(&format!("printf '%s\\n' '{SECRET}' >&2; exit 7")),
        PUBLIC_URL,
        Duration::from_secs(1),
    )
    .unwrap();
    let handle = tunnel.status_handle();
    wait_for_phase(&handle, TunnelPhase::Failed).await;
    assert!(!serde_json::to_string(&handle.snapshot())
        .unwrap()
        .contains(SECRET));
    tunnel.shutdown().await.unwrap();
}

#[test]
fn tracks_each_connection_and_reconnection() {
    let (sender, handle) = status_channel();
    let mut connections = [false; 256];
    for index in [0, 1] {
        observe_line(
            format!(r#"{{"message":"Registered tunnel connection","connIndex":{index}}}"#)
                .as_bytes(),
            &mut connections,
            &sender,
        );
    }
    observe_line(
        br#"{"message":"Unregistered tunnel connection","connIndex":0}"#,
        &mut connections,
        &sender,
    );
    assert_eq!(handle.snapshot().phase, TunnelPhase::Connected);
    observe_line(
        br#"{"message":"Serve tunnel error","connIndex":1}"#,
        &mut connections,
        &sender,
    );
    assert_eq!(handle.snapshot().phase, TunnelPhase::Failed);
    observe_line(
        br#"{"message":"Registered tunnel connection","connIndex":0}"#,
        &mut connections,
        &sender,
    );
    assert_eq!(handle.snapshot().phase, TunnelPhase::Connected);
    observe_line(
        br#"{"message":"Retrying connection in up to 1s","connIndex":0}"#,
        &mut connections,
        &sender,
    );
    assert_eq!(handle.snapshot().phase, TunnelPhase::Failed);
}

#[tokio::test]
async fn oversized_unterminated_log_is_bounded_and_sanitized() {
    let (sender, handle) = status_channel();
    let input = SECRET.repeat(MAX_LOG_BYTES);
    observe_logs(input.as_bytes(), &sender).await;
    let status = handle.snapshot();
    assert_eq!(status.phase, TunnelPhase::Failed);
    assert!(status.message.contains("oversized"));
    assert!(!status.message.contains(SECRET));
}

#[test]
fn drop_kills_and_reaps_after_caller_runtime_stops() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pid");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let tunnel = runtime.block_on(async {
        let tunnel = CloudflareTunnel::start_with_command(
            fake_command(&pid_script(
                &pid_file,
                r#"printf '%s\n' '{"message":"Registered tunnel connection","connIndex":0}' >&2; exec sleep 30"#,
            )),
            PUBLIC_URL, Duration::from_secs(1),
        ).unwrap();
        wait_for_phase(&tunnel.status_handle(), TunnelPhase::Connected).await;
        tunnel
    });
    drop(runtime);
    let handle = tunnel.status_handle();
    drop(tunnel);
    assert_eq!(handle.snapshot().phase, TunnelPhase::Stopped);
    assert_reaped(&pid_file);
}

#[test]
fn preflight_rejects_empty_oversized_and_special_token_files() {
    let directory = tempfile::tempdir().unwrap();
    assert!(validate_token_file(directory.path()).is_err());
    let path = directory.path().join("token");
    std::fs::write(&path, "").unwrap();
    assert!(validate_token_file(&path).is_err());
    std::fs::write(&path, " \n\t").unwrap();
    assert!(validate_token_file(&path).is_err());
    std::fs::write(&path, vec![b'x'; MAX_TOKEN_BYTES as usize + 1]).unwrap();
    assert!(validate_token_file(&path).is_err());
    std::fs::write(&path, SECRET).unwrap();
    validate_token_file(&path).unwrap();
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(validate_token_file(&link).is_err());
}

#[test]
fn version_probe_success_failure_and_timeout_are_sanitized() {
    probe_version(fake_command("exit 0"), Duration::from_secs(1)).unwrap();
    for command in [
        fake_command(&format!("echo '{SECRET}' >&2; exit 9")),
        fake_command("exec sleep 30"),
        Command::new("/nonexistent/cloudflared"),
    ] {
        let error = probe_version(command, Duration::from_millis(50)).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("cloudflared"));
        assert!(!message.contains(SECRET));
    }
}

#[test]
fn phase_wire_values_are_snake_case() {
    for (phase, expected) in [
        (TunnelPhase::Starting, "\"starting\""),
        (TunnelPhase::Connected, "\"connected\""),
        (TunnelPhase::Failed, "\"failed\""),
        (TunnelPhase::Stopped, "\"stopped\""),
    ] {
        assert_eq!(serde_json::to_string(&phase).unwrap(), expected);
        assert_eq!(
            serde_json::from_str::<TunnelPhase>(expected).unwrap(),
            phase
        );
    }
}
