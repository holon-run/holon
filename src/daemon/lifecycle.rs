use std::{
    ffi::OsString,
    fs,
    future::Future,
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;

#[cfg(unix)]
use std::{io::ErrorKind, os::unix::fs::FileTypeExt};

use crate::{
    client::LocalClient,
    config::AppConfig,
    host::RuntimeHost,
    types::{RuntimeFailurePhase, RuntimeFailureSummary},
};

use super::state::latest_known_runtime_failure;
use super::{
    cleanup_daemon_state, clear_persisted_daemon_lifecycle_failures, config_fingerprint,
    daemon_log_hint, daemon_paths, load_daemon_metadata, persist_daemon_lifecycle_failure,
    read_daemon_log_excerpt, runtime_activity_message, stale_files, DaemonLifecycleAction,
    DaemonLifecycleResult, DaemonLifecycleState, DaemonStatusView, RuntimeServiceHandle,
    RuntimeServiceMetadata, RuntimeStatusResponse,
};

const START_TIMEOUT: Duration = Duration::from_secs(10);
const START_STABILITY_WINDOW: Duration = Duration::from_secs(2);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const UNIX_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

pub async fn daemon_status(config: &AppConfig) -> Result<DaemonStatusView> {
    let fingerprint = config_fingerprint(config)?;
    let metadata = load_daemon_metadata(config).ok().flatten();
    let persisted_failure = latest_known_runtime_failure(config).ok().flatten();
    match probe_runtime(config).await {
        ProbeRuntime::Running(status) => Ok(DaemonStatusView {
            ok: true,
            state: DaemonLifecycleState::Running,
            healthy: true,
            home_dir: status.home_dir.clone(),
            socket_path: status.socket_path.clone(),
            http_addr: status.http_addr.clone(),
            pid: Some(status.pid),
            control_connectivity: true,
            runtime_config_fingerprint: Some(status.config_fingerprint.clone()),
            config_fingerprint_match: Some(status.config_fingerprint == fingerprint),
            activity: status.activity.clone(),
            last_failure: merge_latest_failure(status.last_failure.clone(), persisted_failure),
            stale_files: Vec::new(),
            message: status
                .activity
                .as_ref()
                .map(runtime_activity_message)
                .unwrap_or("runtime is healthy")
                .into(),
        }),
        ProbeRuntime::Stopped { occupied_socket } => {
            let stale_files = stale_files(config);
            let pid = metadata.as_ref().map(|record| record.pid);
            let state = if stale_files.is_empty() && !occupied_socket {
                DaemonLifecycleState::Stopped
            } else {
                DaemonLifecycleState::Stale
            };
            let message = if occupied_socket {
                "control socket is occupied by a non-Holon process".into()
            } else if stale_files.is_empty() {
                "runtime is not running".into()
            } else {
                "stale daemon state detected".into()
            };
            Ok(DaemonStatusView {
                ok: true,
                state,
                healthy: false,
                home_dir: config.home_dir.clone(),
                socket_path: config.socket_path.clone(),
                http_addr: config.http_addr.clone(),
                pid,
                control_connectivity: false,
                runtime_config_fingerprint: metadata.map(|record| record.config_fingerprint),
                config_fingerprint_match: None,
                activity: None,
                last_failure: persisted_failure,
                stale_files,
                message,
            })
        }
        ProbeRuntime::Incompatible { details } => Ok(DaemonStatusView {
            ok: true,
            state: DaemonLifecycleState::Stale,
            healthy: false,
            home_dir: config.home_dir.clone(),
            socket_path: config.socket_path.clone(),
            http_addr: config.http_addr.clone(),
            pid: metadata.as_ref().map(|record| record.pid),
            control_connectivity: false,
            runtime_config_fingerprint: metadata.map(|record| record.config_fingerprint),
            config_fingerprint_match: None,
            activity: None,
            last_failure: persisted_failure,
            stale_files: stale_files(config),
            message: format!(
                "runtime is running but incompatible with the daemon lifecycle contract: {details}"
            ),
        }),
    }
}

pub async fn daemon_start(
    config: &AppConfig,
    serve_args: &[OsString],
    control_token_env: Option<&str>,
) -> Result<DaemonLifecycleResult> {
    let current_fingerprint = config_fingerprint(config)?;
    match probe_runtime(config).await {
        ProbeRuntime::Running(status) => {
            if status.home_dir != config.home_dir {
                return Err(anyhow!(
                    "runtime is already running on the configured control surface for a different home: {}",
                    status.home_dir.display()
                ));
            }
            if status.config_fingerprint != current_fingerprint {
                let details = effective_config_mismatch_summary(config, &status);
                return Err(anyhow!(
                    "runtime is already running with a different effective config; use 'holon daemon restart' to replace it; differing config: {details}"
                ));
            }
            let mut status = daemon_status(config).await?;
            status.message = "runtime is already running".into();
            return Ok(DaemonLifecycleResult {
                ok: true,
                action: DaemonLifecycleAction::Start,
                status,
            });
        }
        ProbeRuntime::Stopped {
            occupied_socket: true,
        } => {
            return Err(anyhow!(
                "control socket {} is occupied by a non-Holon process",
                config.socket_path.display()
            ));
        }
        ProbeRuntime::Stopped {
            occupied_socket: false,
        } => {}
        ProbeRuntime::Incompatible { details } => {
            return Err(anyhow!(
                "runtime is already running but incompatible with the daemon lifecycle contract: {details}; use explicit restart after stopping it"
            ));
        }
    }

    cleanup_daemon_state(config)?;
    fs::create_dir_all(config.run_dir())
        .with_context(|| format!("failed to create {}", config.run_dir().display()))?;

    let log_path = daemon_paths(config).log_path;
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .with_context(|| format!("failed to clone {}", log_path.display()))?;
    let exe = std::env::current_exe().context("failed to resolve current holon executable")?;
    let mut command = Command::new(exe);
    command
        .arg("serve")
        .args(serve_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    if let Some(token) = control_token_env {
        command.env("HOLON_CONTROL_TOKEN", token);
    }
    let mut child = command.spawn().context("failed to spawn 'holon serve'")?;

    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        match probe_runtime(config).await {
            ProbeRuntime::Running(status) => {
                if status.config_fingerprint != current_fingerprint {
                    let details = effective_config_mismatch_summary(config, &status);
                    best_effort_cleanup_spawned_start(config, &mut child).await;
                    let _ = persist_daemon_lifecycle_failure(
                        config,
                        &RuntimeFailureSummary {
                            occurred_at: Utc::now(),
                            summary: format!(
                                "daemon start failed because runtime reported a different effective config fingerprint: {details}"
                            ),
                            phase: RuntimeFailurePhase::Startup,
                            detail_hint: Some(daemon_log_hint()),
                            failure_artifact: None,
                        },
                    );
                    return Err(anyhow!(
                        "runtime started but reported a different effective config fingerprint; differing config: {details}; {}",
                        daemon_log_hint()
                    ));
                }
                if let Err(err) =
                    wait_for_startup_stability(config, &mut child, status.pid, &current_fingerprint)
                        .await
                {
                    best_effort_cleanup_spawned_start(config, &mut child).await;
                    let _ = persist_daemon_lifecycle_failure(
                        config,
                        &RuntimeFailureSummary {
                            occurred_at: Utc::now(),
                            summary: format!(
                                "daemon start failed during startup stabilization: {err}"
                            ),
                            phase: RuntimeFailurePhase::Startup,
                            detail_hint: Some(daemon_log_hint()),
                            failure_artifact: None,
                        },
                    );
                    return Err(anyhow!(
                        "daemon start failed during startup stabilization: {err}; {}",
                        daemon_log_hint()
                    ));
                }
                clear_persisted_daemon_lifecycle_failures(config)?;
                return Ok(DaemonLifecycleResult {
                    ok: true,
                    action: DaemonLifecycleAction::Start,
                    status: daemon_status(config).await?,
                });
            }
            ProbeRuntime::Stopped { .. } => {}
            ProbeRuntime::Incompatible { details } => {
                best_effort_cleanup_spawned_start(config, &mut child).await;
                let _ = persist_daemon_lifecycle_failure(
                    config,
                    &RuntimeFailureSummary {
                        occurred_at: Utc::now(),
                        summary: format!(
                            "daemon start failed because runtime reported an incompatible daemon lifecycle contract: {details}"
                        ),
                        phase: RuntimeFailurePhase::Startup,
                        detail_hint: Some(daemon_log_hint()),
                        failure_artifact: None,
                    },
                );
                return Err(anyhow!(
                    "runtime started but reported an incompatible daemon lifecycle contract: {details}; {}",
                    daemon_log_hint()
                ));
            }
        }

        if let Some(exit) = child.try_wait().context("failed to inspect child status")? {
            let details = read_daemon_log_excerpt(config);
            let _ = persist_daemon_lifecycle_failure(
                config,
                &RuntimeFailureSummary {
                    occurred_at: Utc::now(),
                    summary: format!("daemon failed to start; serve exited with status {exit}"),
                    phase: RuntimeFailurePhase::Startup,
                    detail_hint: Some(daemon_log_hint()),
                    failure_artifact: None,
                },
            );
            let _ = cleanup_daemon_state(config);
            return Err(anyhow!(
                "daemon failed to start; serve exited with status {exit}: {details}; {}",
                daemon_log_hint()
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            best_effort_cleanup_spawned_start(config, &mut child).await;
            let _ = persist_daemon_lifecycle_failure(
                config,
                &RuntimeFailureSummary {
                    occurred_at: Utc::now(),
                    summary: format!(
                        "timed out waiting for runtime on {}",
                        config.socket_path.display()
                    ),
                    phase: RuntimeFailurePhase::Startup,
                    detail_hint: Some(daemon_log_hint()),
                    failure_artifact: None,
                },
            );
            return Err(anyhow!(
                "timed out waiting for runtime on {}; {}",
                config.socket_path.display(),
                daemon_log_hint()
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub async fn daemon_stop(config: &AppConfig) -> Result<DaemonLifecycleResult> {
    let before = daemon_status(config).await?;
    let stop_probe = probe_runtime(config).await;
    match &stop_probe {
        ProbeRuntime::Stopped {
            occupied_socket: true,
        } => {
            return Err(anyhow!(
                "control socket {} is occupied by a non-Holon process; refusing to clean it up",
                config.socket_path.display()
            ));
        }
        ProbeRuntime::Incompatible { details } => {
            let _ = persist_daemon_lifecycle_failure(
                config,
                &RuntimeFailureSummary {
                    occurred_at: Utc::now(),
                    summary: format!(
                        "daemon stop failed because runtime was incompatible with the daemon lifecycle contract: {details}"
                    ),
                    phase: RuntimeFailurePhase::Shutdown,
                    detail_hint: Some(daemon_log_hint()),
                    failure_artifact: None,
                },
            );
            return Err(anyhow!(
                "cannot stop runtime: the control socket exists, but the status probe failed.\n\
\n\
This means the runtime cannot be safely controlled by the current `holon daemon` commands. \
The socket may belong to an incompatible or unresponsive runtime, or it may be a stale socket \
with no runtime currently serving it. Blindly stopping based on this state could leave the \
runtime in an inconsistent state.\n\
\n\
What you can do:\n\
  1. Run `holon daemon logs` to see recent runtime activity.\n\
  2. Run `holon daemon status` to see what is known about the runtime.\n\
  3. If the socket appears stale or the runtime is unresponsive, find its PID (from status or \
system tools) and terminate it directly if needed.\n\
  4. Then run `holon daemon start` to launch a compatible runtime.\n\
\n\
Probe error: {details}",
            ));
        }
        _ => {}
    }
    if before.state == DaemonLifecycleState::Stopped && before.stale_files.is_empty() {
        return Ok(DaemonLifecycleResult {
            ok: true,
            action: DaemonLifecycleAction::Stop,
            status: before,
        });
    }

    let shutdown_config = match &stop_probe {
        ProbeRuntime::Running(status) => config_for_runtime_status(config, status),
        _ => config.clone(),
    };
    let client = LocalClient::new(shutdown_config)?;
    let graceful = client.runtime_shutdown().await;
    if graceful.is_ok() && wait_for_shutdown(config, STOP_TIMEOUT).await.is_ok() {
        clear_persisted_daemon_lifecycle_failures(config)?;
        cleanup_daemon_state(config)?;
        return Ok(DaemonLifecycleResult {
            ok: true,
            action: DaemonLifecycleAction::Stop,
            status: stopped_status(config)?,
        });
    }

    #[cfg(unix)]
    {
        if let Some(pid) = before.pid {
            match send_signal(pid, 15, "-TERM")? {
                SignalOutcome::Delivered => {}
                SignalOutcome::PermissionDenied => {}
                SignalOutcome::MissingProcess => {
                    clear_persisted_daemon_lifecycle_failures(config)?;
                    cleanup_daemon_state(config)?;
                    return Ok(DaemonLifecycleResult {
                        ok: true,
                        action: DaemonLifecycleAction::Stop,
                        status: stopped_status(config)?,
                    });
                }
            }
            if wait_for_shutdown(config, STOP_TIMEOUT).await.is_ok() {
                clear_persisted_daemon_lifecycle_failures(config)?;
                cleanup_daemon_state(config)?;
                return Ok(DaemonLifecycleResult {
                    ok: true,
                    action: DaemonLifecycleAction::Stop,
                    status: stopped_status(config)?,
                });
            }
            match send_signal(pid, 9, "-KILL")? {
                SignalOutcome::Delivered | SignalOutcome::MissingProcess => {}
                SignalOutcome::PermissionDenied => {
                    // Neither TERM nor KILL could be delivered — the process
                    // may still be running, so don't delete state files.
                    return Err(anyhow!(
                        "cannot stop daemon PID {pid}: permission denied (TERM and KILL both failed); run as root or the process owner to stop this daemon"
                    ));
                }
            }
        }
    }

    clear_persisted_daemon_lifecycle_failures(config)?;
    cleanup_daemon_state(config)?;
    Ok(DaemonLifecycleResult {
        ok: true,
        action: DaemonLifecycleAction::Stop,
        status: stopped_status(config)?,
    })
}

fn config_for_runtime_status(config: &AppConfig, status: &RuntimeStatusResponse) -> AppConfig {
    let mut runtime_config = config.clone();
    runtime_config.home_dir = status.home_dir.clone();
    runtime_config.socket_path = status.socket_path.clone();
    runtime_config.http_addr = status.http_addr.clone();
    runtime_config
}

fn stopped_status(config: &AppConfig) -> Result<DaemonStatusView> {
    Ok(DaemonStatusView {
        ok: true,
        state: DaemonLifecycleState::Stopped,
        healthy: false,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        pid: None,
        control_connectivity: false,
        runtime_config_fingerprint: None,
        config_fingerprint_match: None,
        activity: None,
        last_failure: latest_known_runtime_failure(config)?,
        stale_files: stale_files(config),
        message: "runtime is not running".into(),
    })
}

pub async fn daemon_restart(
    config: &AppConfig,
    serve_args: &[OsString],
    control_token_env: Option<&str>,
) -> Result<DaemonLifecycleResult> {
    let _ = daemon_stop(config).await?;
    let started = daemon_start(config, serve_args, control_token_env).await?;
    Ok(DaemonLifecycleResult {
        ok: true,
        action: DaemonLifecycleAction::Restart,
        status: started.status,
    })
}

pub async fn ensure_serve_preflight(config: &AppConfig) -> Result<()> {
    let fingerprint = config_fingerprint(config)?;
    match probe_runtime(config).await {
        ProbeRuntime::Running(status) => {
            if status.home_dir != config.home_dir {
                return Err(anyhow!(
                    "runtime is already running on the configured control surface for a different home: {}",
                    status.home_dir.display()
                ));
            }
            if status.config_fingerprint == fingerprint {
                return Err(anyhow!(
                    "runtime is already running for {}; use 'holon daemon status' to inspect it",
                    config.home_dir.display()
                ));
            }
            let details = effective_config_mismatch_summary(config, &status);
            return Err(anyhow!(
                "runtime is already running with a different effective config; stop or restart it explicitly; differing config: {details}"
            ));
        }
        ProbeRuntime::Stopped {
            occupied_socket: true,
        } if recorded_daemon_process_is_missing(config)? => {
            cleanup_daemon_state(config)?;
            Ok(())
        }
        ProbeRuntime::Stopped {
            occupied_socket: true,
        } => Err(anyhow!(
            "control socket {} is occupied by a non-Holon process",
            config.socket_path.display()
        )),
        ProbeRuntime::Stopped {
            occupied_socket: false,
        } => {
            cleanup_daemon_state(config)?;
            Ok(())
        }
        ProbeRuntime::Incompatible { details } => Err(anyhow!(
            "runtime is already running but incompatible with the daemon lifecycle contract: {details}; stop or restart it explicitly"
        )),
    }
}

#[cfg(unix)]
fn recorded_daemon_process_is_missing(config: &AppConfig) -> Result<bool> {
    let Some(metadata) = load_daemon_metadata(config)? else {
        return Ok(false);
    };
    Ok(matches!(
        send_signal(metadata.pid, 0, "0")?,
        SignalOutcome::MissingProcess
    ))
}

#[cfg(not(unix))]
fn recorded_daemon_process_is_missing(_config: &AppConfig) -> Result<bool> {
    Ok(false)
}

pub async fn graceful_runtime_shutdown(
    host: &RuntimeHost,
    runtime_service: &RuntimeServiceHandle,
) -> Result<()> {
    host.shutdown().await?;
    runtime_service.request_shutdown()?;
    Ok(())
}

async fn best_effort_cleanup_spawned_start(config: &AppConfig, child: &mut Child) {
    let child_pid = child.id();
    match probe_runtime(config).await {
        ProbeRuntime::Running(status) => {
            if status.pid == child_pid {
                if let Ok(client) = LocalClient::new(config.clone()) {
                    let _ = client.runtime_shutdown().await;
                    let _ = wait_for_shutdown(config, Duration::from_secs(2)).await;
                }
                if let Ok(None) = child.try_wait() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                let _ = cleanup_daemon_state(config);
                return;
            }

            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return;
        }
        ProbeRuntime::Incompatible { .. } => {
            if let Ok(client) = LocalClient::new(config.clone()) {
                let _ = client.runtime_shutdown().await;
                let _ = wait_for_shutdown(config, Duration::from_secs(2)).await;
            }
            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = cleanup_daemon_state(config);
            return;
        }
        ProbeRuntime::Stopped { occupied_socket } => {
            if occupied_socket {
                if let Ok(None) = child.try_wait() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return;
            }
        }
    }

    if let Ok(None) = child.try_wait() {
        let _ = child.kill();
        let _ = child.wait();
    }

    let _ = cleanup_daemon_state(config);
}

pub(crate) async fn wait_for_startup_stability(
    config: &AppConfig,
    child: &mut Child,
    expected_pid: u32,
    expected_fingerprint: &str,
) -> Result<()> {
    wait_for_startup_stability_with_probe(config, child, expected_pid, expected_fingerprint, || {
        probe_runtime(config)
    })
    .await
}

pub(crate) async fn wait_for_startup_stability_with_probe<F, Fut>(
    config: &AppConfig,
    child: &mut Child,
    expected_pid: u32,
    expected_fingerprint: &str,
    mut probe: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ProbeRuntime>,
{
    let deadline = tokio::time::Instant::now() + START_STABILITY_WINDOW;
    loop {
        if let Some(exit) = child.try_wait().context("failed to inspect child status")? {
            return Err(anyhow!(
                "serve exited with status {exit} before the startup stability window completed"
            ));
        }

        match probe().await {
            ProbeRuntime::Running(status) => {
                if status.pid != expected_pid {
                    return Err(anyhow!(
                        "runtime pid changed during startup stabilization: expected {expected_pid}, got {}",
                        status.pid
                    ));
                }
                if status.config_fingerprint != expected_fingerprint {
                    let details = effective_config_mismatch_summary(config, &status);
                    return Err(anyhow!(
                        "runtime config fingerprint changed during startup stabilization; differing config: {details}"
                    ));
                }
            }
            ProbeRuntime::Stopped { occupied_socket } => {
                if occupied_socket && tokio::time::Instant::now() >= deadline {
                    return Ok(());
                }
                if should_retry_startup_stability_probe(occupied_socket, deadline) {
                    tokio::time::sleep(POLL_INTERVAL).await;
                    continue;
                }
                return Err(anyhow!(
                    "runtime became unreachable during startup stabilization{}",
                    if occupied_socket {
                        "; control socket remained occupied"
                    } else {
                        ""
                    }
                ));
            }
            ProbeRuntime::Incompatible { details } => {
                return Err(anyhow!(
                    "runtime became incompatible during startup stabilization: {details}"
                ));
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub(crate) fn should_retry_startup_stability_probe(
    occupied_socket: bool,
    deadline: tokio::time::Instant,
) -> bool {
    occupied_socket && tokio::time::Instant::now() < deadline
}

async fn wait_for_shutdown(config: &AppConfig, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match probe_runtime(config).await {
            ProbeRuntime::Running(_) => {}
            ProbeRuntime::Stopped { .. } => return Ok(()),
            ProbeRuntime::Incompatible { details } => {
                return Err(anyhow!(
                    "runtime remained reachable but incompatible during shutdown: {details}"
                ));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for runtime shutdown"));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

pub(crate) fn effective_config_mismatch_summary(
    config: &AppConfig,
    status: &RuntimeStatusResponse,
) -> String {
    let mut differences = Vec::new();
    push_diff(
        &mut differences,
        "home_dir",
        config.home_dir.display().to_string(),
        status.home_dir.display().to_string(),
    );
    push_diff(
        &mut differences,
        "socket_path",
        config.socket_path.display().to_string(),
        status.socket_path.display().to_string(),
    );
    push_diff(
        &mut differences,
        "http_addr",
        config.http_addr.clone(),
        status.http_addr.clone(),
    );

    if let Some(startup) = &status.startup_surface {
        push_diff(
            &mut differences,
            "workspace_dir",
            config.workspace_dir.display().to_string(),
            startup.workspace_dir.display().to_string(),
        );
        push_diff(
            &mut differences,
            "default_agent_id",
            config.default_agent_id.clone(),
            startup.default_agent_id.clone(),
        );
        push_diff(
            &mut differences,
            "callback_base_url",
            config.callback_base_url.clone(),
            startup.callback_base_url.clone(),
        );
        push_diff(
            &mut differences,
            "control_auth_mode",
            format!("{:?}", config.control_auth_mode),
            format!("{:?}", startup.control_auth_mode),
        );
        push_diff(
            &mut differences,
            "control_token_configured",
            config.control_token.is_some().to_string(),
            startup.control_token_configured.to_string(),
        );
    } else {
        differences.push("startup_surface missing from runtime status".into());
    }

    if let Some(runtime) = &status.runtime_surface {
        push_diff(
            &mut differences,
            "model.default",
            config.default_model.as_string(),
            runtime.model_default.clone(),
        );
        push_diff(
            &mut differences,
            "model.fallbacks",
            config
                .fallback_models
                .iter()
                .map(|model| model.as_string())
                .collect::<Vec<_>>()
                .join(","),
            runtime.model_fallbacks.join(","),
        );
        push_diff(
            &mut differences,
            "runtime_max_output_tokens",
            config.runtime_max_output_tokens.to_string(),
            runtime.runtime_max_output_tokens.to_string(),
        );
        push_diff(
            &mut differences,
            "default_tool_output_tokens",
            config.default_tool_output_tokens.to_string(),
            runtime.default_tool_output_tokens.to_string(),
        );
        push_diff(
            &mut differences,
            "max_tool_output_tokens",
            config.max_tool_output_tokens.to_string(),
            runtime.max_tool_output_tokens.to_string(),
        );
        push_diff(
            &mut differences,
            "disable_provider_fallback",
            config.provider_fallback_disabled().to_string(),
            runtime.disable_provider_fallback.to_string(),
        );
    } else {
        differences.push("runtime_surface missing from runtime status".into());
    }

    if differences.is_empty() {
        "fingerprint differed, but no field-level difference was available from runtime status"
            .into()
    } else {
        differences.join("; ")
    }
}

fn push_diff(differences: &mut Vec<String>, key: &str, expected: String, actual: String) {
    if expected != actual {
        differences.push(format!("{key} expected={expected:?} actual={actual:?}"));
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalOutcome {
    Delivered,
    MissingProcess,
    PermissionDenied,
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: i32, signal_name: &str) -> Result<SignalOutcome> {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    const ESRCH: i32 = 3;
    const EPERM: i32 = 1;
    let result = unsafe { kill(pid as i32, signal) };
    if result == 0 {
        return Ok(SignalOutcome::Delivered);
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ESRCH) {
        return Ok(SignalOutcome::MissingProcess);
    }
    if err.raw_os_error() == Some(EPERM) {
        return Ok(SignalOutcome::PermissionDenied);
    }
    Err(anyhow!("kill {signal_name} {pid} failed: {err}"))
}

#[derive(Debug)]
pub(crate) enum ProbeRuntime {
    Running(Box<RuntimeStatusResponse>),
    Stopped { occupied_socket: bool },
    Incompatible { details: String },
}

pub(crate) async fn probe_runtime(config: &AppConfig) -> ProbeRuntime {
    #[cfg(unix)]
    if config.socket_path.exists() {
        if let Ok(metadata) = fs::symlink_metadata(&config.socket_path) {
            if !metadata.file_type().is_socket() {
                return ProbeRuntime::Stopped {
                    occupied_socket: false,
                };
            }
        }
        let client = match LocalClient::new(config.clone()) {
            Ok(client) => client,
            Err(_) => {
                return ProbeRuntime::Stopped {
                    occupied_socket: false,
                }
            }
        };
        match tokio::time::timeout(UNIX_PROBE_TIMEOUT, client.runtime_readiness_unix_only()).await {
            Ok(Ok(status)) => return ProbeRuntime::Running(Box::new(status)),
            Ok(Err(err)) => {
                return match unix_probe_stopped_socket_occupancy(err.root_cause()) {
                    Some(occupied_socket) => ProbeRuntime::Stopped { occupied_socket },
                    None => ProbeRuntime::Incompatible {
                        details: err.to_string(),
                    },
                };
            }
            Err(_) => {
                return ProbeRuntime::Stopped {
                    occupied_socket: true,
                };
            }
        }
    }

    // Fallback: when the unix socket is missing but the PID file records a
    // live process, the daemon is still running (the socket may have been
    // removed externally — see https://github.com/holon-run/holon/issues/1448).
    if let Ok(Some(metadata)) = load_daemon_metadata(config) {
        if pid_is_alive(metadata.pid) {
            // Try TCP as a secondary probe, but only trust a response that
            // identifies the daemon recorded in metadata.
            let client = match LocalClient::new(config.clone()) {
                Ok(client) => client,
                Err(_) => {
                    return ProbeRuntime::Running(Box::new(status_from_metadata(metadata)));
                }
            };
            if let Ok(status) = client.runtime_readiness().await {
                if runtime_status_matches_metadata(&status, &metadata) {
                    return ProbeRuntime::Running(Box::new(status));
                }
            }
            // TCP readiness failed or reached a different runtime; report
            // Running with metadata so callers keep the recorded daemon process
            // as the lifecycle anchor.
            return ProbeRuntime::Running(Box::new(status_from_metadata(metadata)));
        }
    }

    let client = match LocalClient::new(config.clone()) {
        Ok(client) => client,
        Err(_) => {
            return ProbeRuntime::Stopped {
                occupied_socket: false,
            }
        }
    };
    match client.runtime_readiness().await {
        Ok(status) => ProbeRuntime::Running(Box::new(status)),
        Err(_) => ProbeRuntime::Stopped {
            occupied_socket: false,
        },
    }
}

pub(crate) fn runtime_status_matches_metadata(
    status: &RuntimeStatusResponse,
    metadata: &RuntimeServiceMetadata,
) -> bool {
    status.pid == metadata.pid
        && status.home_dir == metadata.home_dir
        && status.socket_path == metadata.socket_path
        && status.config_fingerprint == metadata.config_fingerprint
}

fn status_from_metadata(metadata: RuntimeServiceMetadata) -> RuntimeStatusResponse {
    RuntimeStatusResponse {
        ok: true,
        healthy: false,
        home_dir: metadata.home_dir,
        socket_path: metadata.socket_path,
        http_addr: metadata.http_addr,
        pid: metadata.pid,
        started_at: metadata.started_at,
        config_fingerprint: metadata.config_fingerprint,
        activity: None,
        startup_surface: None,
        runtime_surface: None,
        last_failure: None,
    }
}

#[cfg(unix)]
fn unix_probe_stopped_socket_occupancy(root: &(dyn std::error::Error + 'static)) -> Option<bool> {
    if let Some(io_error) = root.downcast_ref::<std::io::Error>() {
        return Some(!matches!(
            io_error.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionAborted
                | ErrorKind::ConnectionReset
                | ErrorKind::NotFound
        ));
    }
    None
}

/// Check whether a process with the given PID is still alive.
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        match send_signal(pid, 0, "0") {
            Ok(SignalOutcome::Delivered) | Ok(SignalOutcome::PermissionDenied) => true,
            Ok(SignalOutcome::MissingProcess) => false,
            // Conservative: on unknown error assume the process may still exist.
            Err(_) => true,
        }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn merge_latest_failure(
    left: Option<RuntimeFailureSummary>,
    right: Option<RuntimeFailureSummary>,
) -> Option<RuntimeFailureSummary> {
    match (left, right) {
        (Some(left), Some(right)) => {
            if left.occurred_at >= right.occurred_at {
                Some(left)
            } else {
                Some(right)
            }
        }
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}
