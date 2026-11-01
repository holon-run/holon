//! Supervision for a preconfigured, remotely managed Cloudflare Named Tunnel.
//!
//! Registration describes the cloudflared-to-edge connection, not end-to-end
//! reachability of the public URL. This module never creates or restarts tunnels.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{oneshot, watch};

const MAX_TOKEN_BYTES: u64 = 16 * 1024;
const MAX_LOG_BYTES: usize = 16 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);
const INSTALL_GUIDANCE: &str =
    "cloudflared is required; install it using Cloudflare's package instructions and ensure it is on PATH";
const CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SystemRoot",
    "WINDIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TunnelPhase {
    Starting,
    Connected,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelStatus {
    pub phase: TunnelPhase,
    pub public_url: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct TunnelStatusHandle {
    receiver: watch::Receiver<TunnelStatus>,
}

impl TunnelStatusHandle {
    pub fn failed(public_url: &str) -> Self {
        let (_, receiver) = watch::channel(TunnelStatus {
            phase: TunnelPhase::Failed,
            public_url: public_url.to_owned(),
            message: "Could not start cloudflared; local service remains available".to_owned(),
        });
        Self { receiver }
    }

    pub fn snapshot(&self) -> TunnelStatus {
        self.receiver.borrow().clone()
    }
}

/// Owns a supervisor with its own reactor so Drop also reaps the process after
/// the caller's Tokio runtime has stopped. There is no restart loop.
pub struct CloudflareTunnel {
    status: TunnelStatusHandle,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl CloudflareTunnel {
    pub fn preflight(token_file: &Path) -> Result<()> {
        validate_token_file(token_file)?;
        probe_version(Command::new("cloudflared"), VERSION_TIMEOUT)
    }

    pub fn start(token_file: &Path, public_url: &str) -> Result<Self> {
        validate_token_file(token_file)?;
        let command = tunnel_command(token_file);
        Self::start_with_command(command, public_url, STARTUP_TIMEOUT)
    }

    fn start_with_command(
        mut command: Command,
        public_url: &str,
        startup_timeout: Duration,
    ) -> Result<Self> {
        tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow!("a Tokio runtime is required to start cloudflared"))?;
        configure_child(&mut command);
        command.stderr(Stdio::piped());
        let (status_tx, status_rx) = watch::channel(TunnelStatus {
            phase: TunnelPhase::Starting,
            public_url: public_url.to_owned(),
            message: "Waiting for cloudflared to register a tunnel connection".to_owned(),
        });
        let (stop_tx, stop_rx) = oneshot::channel();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("cloudflared-supervisor".to_owned())
            .spawn(move || {
                let runtime = match supervisor_runtime() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = started_tx.send(false);
                        return Err(error);
                    }
                };
                runtime.block_on(async move {
                    let mut child = match command.spawn() {
                        Ok(child) => child,
                        Err(_) => {
                            let _ = started_tx.send(false);
                            bail!("could not start cloudflared; {INSTALL_GUIDANCE}");
                        }
                    };
                    let stderr = child.stderr.take().expect("cloudflared stderr is piped");
                    let _ = started_tx.send(true);
                    supervise(child, stderr, status_tx, stop_rx, startup_timeout).await
                })
            })
            .map_err(|_| anyhow!("could not start the cloudflared supervisor"))?;
        if started_rx.recv() != Ok(true) {
            let _ = join_worker(worker);
            bail!("could not start cloudflared; {INSTALL_GUIDANCE}");
        }
        Ok(Self {
            status: TunnelStatusHandle {
                receiver: status_rx,
            },
            stop: Some(stop_tx),
            worker: Some(worker),
        })
    }

    pub fn status_handle(&self) -> TunnelStatusHandle {
        self.status.clone()
    }

    pub async fn shutdown(mut self) -> Result<()> {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || join_worker(worker))
                .await
                .map_err(|_| anyhow!("could not join the cloudflared supervisor"))??;
        }
        Ok(())
    }

    fn request_stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

impl Drop for CloudflareTunnel {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            // The independent supervisor can kill and wait without this reactor.
            let _ = join_worker(worker);
        }
    }
}

fn supervisor_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| anyhow!("could not create the cloudflared supervisor runtime"))
}

fn join_worker(worker: JoinHandle<Result<()>>) -> Result<()> {
    worker
        .join()
        .map_err(|_| anyhow!("the cloudflared supervisor terminated unexpectedly"))?
}

fn validate_token_file(path: &Path) -> Result<()> {
    // Check before opening so special files cannot block an ordinary open.
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| anyhow!("could not inspect the Cloudflare tunnel token file"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_TOKEN_BYTES {
        bail!("the Cloudflare tunnel token file must be a nonempty regular file of at most 16 KiB");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| anyhow!("could not open the Cloudflare tunnel token file"))?;
    let metadata = file
        .metadata()
        .map_err(|_| anyhow!("could not inspect the opened Cloudflare tunnel token file"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_TOKEN_BYTES {
        bail!("the Cloudflare tunnel token file must be a nonempty regular file of at most 16 KiB");
    }
    let mut reader = file.take(MAX_TOKEN_BYTES + 1);
    let mut buffer = [0; 1024];
    let mut bytes = 0;
    let mut non_whitespace = false;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| anyhow!("could not read the Cloudflare tunnel token file"))?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        non_whitespace |= buffer[..count]
            .iter()
            .any(|byte| !byte.is_ascii_whitespace());
    }
    if !non_whitespace || bytes > MAX_TOKEN_BYTES {
        bail!("the Cloudflare tunnel token file must contain a token of at most 16 KiB");
    }
    Ok(())
}

fn tunnel_command(token_file: &Path) -> Command {
    let mut command = Command::new("cloudflared");
    // Ignore ambient config, which could override credentials or enable a
    // sensitive logfile. Remote ingress still comes from the Named Tunnel.
    let empty_config = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .args([
            "tunnel",
            "--config",
            empty_config,
            "--no-autoupdate",
            "--output",
            "json",
            "run",
            "--token-file",
        ])
        .arg(token_file);
    command
}

fn configure_child(command: &mut Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        // Do not inherit runtime authority, model credentials, or tunnel tokens.
        .env_clear();
    for name in CHILD_ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let parent_pid = std::process::id() as libc::pid_t;
        // Only async-signal-safe syscalls are used between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent_pid {
                    return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
    }
}

fn probe_version(mut command: Command, timeout: Duration) -> Result<()> {
    configure_child(&mut command);
    command.arg("--version");
    let worker = std::thread::Builder::new()
        .name("cloudflared-preflight".to_owned())
        .spawn(move || {
            let runtime = supervisor_runtime()?;
            runtime.block_on(async move {
                let mut child = command.spawn().map_err(|_| anyhow!(INSTALL_GUIDANCE))?;
                match tokio::time::timeout(timeout, child.wait()).await {
                    Ok(Ok(status)) if status.success() => Ok(()),
                    Ok(_) => bail!("cloudflared --version failed; {INSTALL_GUIDANCE}"),
                    Err(_) => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        bail!("cloudflared --version timed out; {INSTALL_GUIDANCE}")
                    }
                }
            })
        })
        .map_err(|_| anyhow!("could not start the cloudflared preflight"))?;
    join_worker(worker)
}

fn set_phase(status: &watch::Sender<TunnelStatus>, phase: TunnelPhase, message: &str) {
    status.send_modify(|status| {
        status.phase = phase;
        status.message = message.to_owned();
    });
}

async fn supervise(
    mut child: tokio::process::Child,
    stderr: tokio::process::ChildStderr,
    status: watch::Sender<TunnelStatus>,
    mut stop: oneshot::Receiver<()>,
    startup_timeout: Duration,
) -> Result<()> {
    let logs = observe_logs(stderr, &status);
    tokio::pin!(logs);
    let startup = tokio::time::sleep(startup_timeout);
    tokio::pin!(startup);
    let mut startup_pending = true;
    let mut logs_open = true;
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => {
                let killed = child.kill().await;
                let waited = child.wait().await;
                if killed.is_err() || waited.is_err() {
                    set_phase(&status, TunnelPhase::Failed, "Could not stop the cloudflared process");
                    bail!("could not stop and reap the cloudflared process");
                }
                set_phase(&status, TunnelPhase::Stopped, "Cloudflared stopped");
                return Ok(());
            }
            _ = child.wait() => {
                set_phase(&status, TunnelPhase::Failed, "Cloudflared exited; local service remains available");
                return Ok(());
            }
            _ = &mut startup, if startup_pending => {
                startup_pending = false;
                if status.borrow().phase == TunnelPhase::Starting {
                    set_phase(&status, TunnelPhase::Failed, "Cloudflared did not register a connection within the startup timeout");
                }
            }
            _ = &mut logs, if logs_open => {
                logs_open = false;
                set_phase(&status, TunnelPhase::Failed, "Cloudflared connection observation ended");
            }
        }
    }
}

async fn observe_logs(mut reader: impl AsyncRead + Unpin, status: &watch::Sender<TunnelStatus>) {
    let mut connections = [false; 256];
    let mut chunk = [0; 4096];
    let mut line = Vec::with_capacity(MAX_LOG_BYTES);
    let mut discarding = false;
    loop {
        let count = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                if !discarding {
                    observe_line(&line, &mut connections, status);
                }
                line.clear();
                discarding = false;
            } else if !discarding {
                if line.len() == MAX_LOG_BYTES {
                    line.clear();
                    discarding = true;
                    connections.fill(false);
                    set_phase(
                        status,
                        TunnelPhase::Failed,
                        "Cloudflared emitted an oversized log record",
                    );
                } else {
                    line.push(byte);
                }
            }
        }
    }
}

fn observe_line(line: &[u8], connections: &mut [bool; 256], status: &watch::Sender<TunnelStatus>) {
    // Never retain or publish raw logs, including cloudflared error details.
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    let Some(message) = record.get("message").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(index) = record
        .get("connIndex")
        .and_then(|value| value.as_u64())
        .filter(|index| *index <= u8::MAX as u64)
    else {
        return;
    };
    if message == "Registered tunnel connection" {
        connections[index as usize] = true;
        set_phase(
            status,
            TunnelPhase::Connected,
            "Cloudflared registered an edge connection; public reachability is not verified",
        );
    } else if message == "Unregistered tunnel connection"
        || message == "Serve tunnel error"
        || message.starts_with("Retrying connection in up to ")
        || message.starts_with("Switching to fallback protocol ")
    {
        connections[index as usize] = false;
        if !connections.iter().any(|connected| *connected) {
            set_phase(
                status,
                TunnelPhase::Failed,
                "Cloudflared has no registered edge connections",
            );
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
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
}
