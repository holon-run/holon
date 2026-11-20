//! Supervision for a preconfigured, remotely managed Cloudflare Named Tunnel.
//!
//! Registration describes the cloudflared-to-edge connection, not end-to-end
//! reachability of the public URL. This module never creates or restarts tunnels.

pub use holon_client::wire::{TunnelPhase, TunnelStatus};

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
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
mod tests;
