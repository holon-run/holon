use super::lifecycle::{
    effective_config_mismatch_summary, probe_runtime, runtime_status_matches_metadata,
    should_retry_startup_stability_probe, wait_for_startup_stability_with_probe, ProbeRuntime,
};
use super::state::{
    persist_last_runtime_failure, DAEMON_LOG_TAIL_LINE_CHAR_LIMIT, DAEMON_LOG_TAIL_READ_BYTE_LIMIT,
};
use super::{
    clear_persisted_daemon_lifecycle_failures, config_fingerprint, daemon_log_hint, daemon_logs,
    daemon_paths, daemon_status, daemon_stop, ensure_serve_preflight, load_last_runtime_failure,
    persist_daemon_lifecycle_failure, runtime_activity_summary, DaemonLifecycleState,
    RuntimeActivityState, RuntimeConfigSurface, RuntimeControlAuthMode, RuntimeServiceMetadata,
    RuntimeStartupSurface, RuntimeStatusResponse,
};
use crate::config::{provider_registry_for_tests, AppConfig, ModelRef, ProviderId};
use crate::{
    host::RuntimeHost,
    provider::StubProvider,
    types::{AuthorityClass, CommandTaskSpec, RuntimeFailurePhase, RuntimeFailureSummary},
};
use chrono::Utc;
use std::{fs, process::Command, sync::Arc};
use tempfile::tempdir;

fn test_config() -> AppConfig {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        home_dir: home.path().to_path_buf(),
        data_dir: home.path().to_path_buf(),
        socket_path: home.path().join("run").join("holon.sock"),
        workspace_dir: workspace.path().to_path_buf(),
        context_window_messages: 8,
        context_window_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens: 4096,
        compaction_trigger_estimated_tokens: 2048,
        compaction_keep_recent_estimated_tokens: 768,
        recent_episode_candidates: 12,
        max_relevant_episodes: 3,
        control_token: Some("secret".into()),
        control_auth_mode: crate::config::ControlAuthMode::Auto,
        config_file_path: home.path().join("config.json"),
        stored_config: Default::default(),
        default_model: ModelRef {
            provider: ProviderId::anthropic(),
            model: "claude-sonnet-4-6".into(),
        },
        fallback_models: vec![],
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
        disable_provider_fallback: false,
        tui_alternate_screen: crate::config::AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        model_discovery_cache: Default::default(),
        providers: provider_registry_for_tests(None, Some("dummy"), home.path().join(".codex")),
        web_config: crate::web::WebConfig::default(),
    }
}

#[cfg(unix)]
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn config_fingerprint_changes_when_effective_config_changes() {
    let mut left = test_config();
    let right = left.clone();
    left.http_addr = "127.0.0.1:9999".into();
    assert_ne!(
        config_fingerprint(&left).unwrap(),
        config_fingerprint(&right).unwrap()
    );
}

#[test]
fn daemon_paths_use_run_dir_convention() {
    let config = test_config();
    let paths = daemon_paths(&config);
    assert_eq!(paths.pid_path, config.run_dir().join("holon.pid"));
    assert_eq!(paths.metadata_path, config.run_dir().join("daemon.json"));
    assert_eq!(paths.log_path, config.run_dir().join("daemon.log"));
    assert_eq!(
        paths.last_failure_path,
        config.run_dir().join("last_failure.json")
    );
    assert_eq!(
        paths.startup_failure_path,
        config.run_dir().join("startup_failure.json")
    );
    assert_eq!(
        paths.shutdown_failure_path,
        config.run_dir().join("shutdown_failure.json")
    );
}

#[test]
fn runtime_service_metadata_round_trips() {
    let config = test_config();
    let metadata = RuntimeServiceMetadata {
        pid: 42,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    let encoded = serde_json::to_vec(&metadata).unwrap();
    let decoded: RuntimeServiceMetadata = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.pid, 42);
    assert_eq!(decoded.home_dir, config.home_dir);
}

#[test]
fn persisted_last_runtime_failure_round_trips() {
    let config = test_config();
    let failure = RuntimeFailureSummary {
        occurred_at: Utc::now(),
        summary: "daemon start failed".into(),
        phase: RuntimeFailurePhase::Startup,
        detail_hint: Some(daemon_log_hint()),
        failure_artifact: None,
    };
    persist_last_runtime_failure(&config, &failure).unwrap();
    let loaded = load_last_runtime_failure(&config).unwrap();
    assert_eq!(loaded, Some(failure));
}

#[test]
fn persisted_daemon_lifecycle_failures_clear_after_success() {
    let config = test_config();
    let failure = RuntimeFailureSummary {
        occurred_at: Utc::now(),
        summary: "daemon stop failed".into(),
        phase: RuntimeFailurePhase::Shutdown,
        detail_hint: Some(daemon_log_hint()),
        failure_artifact: None,
    };
    persist_daemon_lifecycle_failure(&config, &failure).unwrap();
    clear_persisted_daemon_lifecycle_failures(&config).unwrap();
    assert_eq!(load_last_runtime_failure(&config).unwrap(), None);
    assert!(!daemon_paths(&config).shutdown_failure_path.exists());
}

#[test]
fn daemon_logs_surface_paths_failures_and_tail() {
    let config = test_config();
    let failure = RuntimeFailureSummary {
        occurred_at: Utc::now(),
        summary: "daemon start failed".into(),
        phase: RuntimeFailurePhase::Startup,
        detail_hint: Some(daemon_log_hint()),
        failure_artifact: None,
    };
    persist_daemon_lifecycle_failure(&config, &failure).unwrap();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(
        &paths.log_path,
        "first line\nsecond line\nthird line\nfourth line\n",
    )
    .unwrap();

    let view = daemon_logs(&config, 2).unwrap();
    assert_eq!(view.log_path, paths.log_path);
    assert_eq!(view.metadata_path, paths.metadata_path);
    assert_eq!(view.last_failure_path, paths.last_failure_path);
    assert_eq!(view.startup_failure_path, paths.startup_failure_path);
    assert_eq!(view.shutdown_failure_path, paths.shutdown_failure_path);
    assert_eq!(view.last_failure, Some(failure.clone()));
    assert_eq!(view.startup_failure, Some(failure));
    assert_eq!(view.shutdown_failure, None);
    assert_eq!(
        view.tail,
        vec!["third line".to_string(), "fourth line".to_string()]
    );
}

#[test]
fn daemon_logs_tail_zero_omits_log_tail() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.log_path, "first line\nsecond line\n").unwrap();

    let view = daemon_logs(&config, 0).unwrap();
    assert!(view.tail.is_empty());
    assert_eq!(view.message, "daemon log tail omitted (--tail 0)");
}

#[test]
fn daemon_logs_tail_stays_bounded_for_large_log_lines() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    let huge_line = format!(
        "{}tail-marker",
        "x".repeat(DAEMON_LOG_TAIL_READ_BYTE_LIMIT + 4_096)
    );
    fs::write(&paths.log_path, format!("old line\n{huge_line}\n")).unwrap();

    let view = daemon_logs(&config, 1).unwrap();
    assert_eq!(view.tail.len(), 1);
    assert!(view.tail[0].starts_with("..."));
    assert!(view.tail[0].ends_with("tail-marker"));
    assert!(view.tail[0].chars().count() <= DAEMON_LOG_TAIL_LINE_CHAR_LIMIT);
}

#[tokio::test]
async fn daemon_status_surfaces_persisted_last_failure_when_runtime_stopped() {
    let config = test_config();
    let failure = RuntimeFailureSummary {
        occurred_at: Utc::now(),
        summary: "daemon start failed".into(),
        phase: RuntimeFailurePhase::Startup,
        detail_hint: Some(daemon_log_hint()),
        failure_artifact: None,
    };
    persist_last_runtime_failure(&config, &failure).unwrap();
    let status = daemon_status(&config).await.unwrap();
    assert_eq!(status.state, DaemonLifecycleState::Stopped);
    assert_eq!(status.last_failure, Some(failure));
}

#[test]
fn runtime_status_response_decodes_without_activity_field() {
    let payload = serde_json::json!({
        "ok": true,
        "healthy": true,
        "pid": 42,
        "home_dir": "/tmp/holon",
        "socket_path": "/tmp/holon.sock",
        "http_addr": "127.0.0.1:1234",
        "started_at": "2026-04-15T00:00:00Z",
        "config_fingerprint": "abc123"
    });
    let decoded: RuntimeStatusResponse = serde_json::from_value(payload).unwrap();
    assert_eq!(decoded.pid, 42);
    assert!(decoded.activity.is_none());
}

#[test]
fn runtime_status_response_decodes_startup_surface_without_callback_base_url() {
    let payload = serde_json::json!({
        "ok": true,
        "healthy": true,
        "pid": 42,
        "home_dir": "/tmp/holon",
        "socket_path": "/tmp/holon.sock",
        "http_addr": "127.0.0.1:1234",
        "started_at": "2026-04-15T00:00:00Z",
        "config_fingerprint": "abc123",
        "startup_surface": {
            "home_dir": "/tmp/holon",
            "socket_path": "/tmp/holon.sock",
            "workspace_dir": "/tmp/workspace",
            "default_agent_id": "main",
            "control_token_configured": false,
            "control_auth_mode": "auto"
        }
    });
    let decoded: RuntimeStatusResponse = serde_json::from_value(payload).unwrap();
    assert_eq!(
        decoded
            .startup_surface
            .expect("startup surface should decode")
            .callback_base_url,
        ""
    );
}

#[test]
fn effective_config_mismatch_summary_lists_actionable_differences() {
    let mut expected = test_config();
    expected.http_addr = "0.0.0.0:7878".into();
    expected.callback_base_url = "http://192.168.1.10:7878".into();
    expected.control_auth_mode = crate::config::ControlAuthMode::Required;

    let mut actual_surface = RuntimeConfigSurface::new(&expected);
    actual_surface.runtime_max_output_tokens = expected.runtime_max_output_tokens;
    let status = RuntimeStatusResponse {
        ok: true,
        healthy: true,
        pid: 42,
        home_dir: expected.home_dir.clone(),
        socket_path: expected.socket_path.clone(),
        http_addr: "127.0.0.1:7878".into(),
        started_at: Utc::now(),
        config_fingerprint: "actual".into(),
        activity: None,
        startup_surface: Some(RuntimeStartupSurface {
            home_dir: expected.home_dir.clone(),
            socket_path: expected.socket_path.clone(),
            workspace_dir: expected.workspace_dir.clone(),
            default_agent_id: expected.default_agent_id.clone(),
            callback_base_url: "http://127.0.0.1:7878".into(),
            control_token_configured: false,
            control_auth_mode: RuntimeControlAuthMode::Auto,
        }),
        runtime_surface: Some(actual_surface),
        last_failure: None,
    };

    let summary = effective_config_mismatch_summary(&expected, &status);
    assert!(summary.contains("http_addr expected=\"0.0.0.0:7878\" actual=\"127.0.0.1:7878\""));
    assert!(summary.contains(
        "callback_base_url expected=\"http://192.168.1.10:7878\" actual=\"http://127.0.0.1:7878\""
    ));
    assert!(summary.contains("control_auth_mode expected=\"Required\" actual=\"Auto\""));
    assert!(summary.contains("control_token_configured expected=\"true\" actual=\"false\""));
    assert!(!summary.contains("secret"));
}

#[tokio::test]
async fn runtime_activity_summary_reports_idle_runtime() {
    let config = test_config();
    fs::create_dir_all(&config.workspace_dir).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
    let _runtime = host.default_runtime().await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let summary = runtime_activity_summary(&host).await.unwrap();
        if summary.state == RuntimeActivityState::Idle {
            assert_eq!(summary.active_agent_count, 1);
            assert_eq!(summary.active_task_count, 0);
            assert_eq!(summary.processing_agent_count, 0);
            assert_eq!(summary.waiting_agent_count, 0);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn runtime_activity_summary_reports_waiting_runtime() {
    let config = test_config();
    fs::create_dir_all(&config.workspace_dir).unwrap();
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("ok"))).unwrap();
    let runtime = host.default_runtime().await.unwrap();
    let _task = runtime
        .schedule_command_task(
            "daemon wait".into(),
            CommandTaskSpec {
                cmd: "sleep 1".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::OperatorInstruction,
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let summary = runtime_activity_summary(&host).await.unwrap();
        if summary.state == RuntimeActivityState::Waiting && summary.active_task_count >= 1 {
            assert_eq!(summary.active_agent_count, 1);
            assert!(summary.waiting_agent_count >= 1);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn probe_runtime_treats_non_socket_path_as_stale() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&config.socket_path, b"not a socket").unwrap();
    match probe_runtime(&config).await {
        ProbeRuntime::Stopped { occupied_socket } => assert!(!occupied_socket),
        ProbeRuntime::Running(_) => panic!("expected stale runtime probe"),
        ProbeRuntime::Incompatible { details } => {
            panic!("expected stale runtime probe, got incompatible: {details}")
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn probe_runtime_reports_running_when_socket_missing_but_pid_alive() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    // Use the current process PID — guaranteed alive.
    let pid = std::process::id();
    fs::write(&paths.pid_path, format!("{pid}\n")).unwrap();
    // Use metadata paths that differ from current config to verify the
    // fallback returns persisted metadata, not caller config.
    let metadata_home = config.home_dir.join("persisted-home");
    let metadata_socket = metadata_home.join("run").join("holon.sock");
    let metadata_http = "127.0.0.1:19999".to_string();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: metadata_home.clone(),
        socket_path: metadata_socket.clone(),
        http_addr: metadata_http.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    // Intentionally do NOT create the socket — simulate externally removed socket.
    match probe_runtime(&config).await {
        ProbeRuntime::Running(status) => {
            assert_eq!(status.pid, pid);
            assert_eq!(status.home_dir, metadata_home);
            assert_eq!(status.socket_path, metadata_socket);
            assert_eq!(status.http_addr, metadata_http);
        }
        other => panic!("expected Running, got {:?}", other),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn probe_runtime_reports_running_when_socket_refuses_but_pid_alive() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    drop(listener);

    // Use the current process PID — guaranteed alive.
    let pid = std::process::id();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    match probe_runtime(&config).await {
        ProbeRuntime::Running(status) => {
            assert_eq!(status.pid, pid);
            assert_eq!(status.home_dir, config.home_dir);
            assert_eq!(status.socket_path, config.socket_path);
            assert_eq!(status.http_addr, config.http_addr);
        }
        other => panic!("expected Running, got {:?}", other),
    }
}

#[test]
fn runtime_status_metadata_match_rejects_foreign_runtime() {
    let config = test_config();
    let metadata = RuntimeServiceMetadata {
        pid: 1234,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    let matching_status = RuntimeStatusResponse {
        ok: true,
        healthy: true,
        pid: metadata.pid,
        home_dir: metadata.home_dir.clone(),
        socket_path: metadata.socket_path.clone(),
        http_addr: metadata.http_addr.clone(),
        started_at: metadata.started_at,
        config_fingerprint: metadata.config_fingerprint.clone(),
        activity: None,
        startup_surface: None,
        runtime_surface: None,
        last_failure: None,
    };
    assert!(runtime_status_matches_metadata(&matching_status, &metadata));

    let mut foreign_status = matching_status;
    foreign_status.home_dir = config.home_dir.join("foreign-home");
    assert!(!runtime_status_matches_metadata(&foreign_status, &metadata));
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_status_surfaces_dead_pid_and_leftover_socket_as_stale() {
    let config = test_config();
    let paths = daemon_paths(&config);
    let pid = dead_pid();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.pid_path, format!("{pid}\n")).unwrap();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    drop(listener);

    let status = daemon_status(&config).await.unwrap();

    assert_eq!(status.state, DaemonLifecycleState::Stale);
    assert_eq!(status.pid, Some(pid));
    assert!(!status.control_connectivity);
    assert_eq!(status.message, "stale daemon state detected");
    assert!(status.stale_files.contains(&paths.pid_path));
    assert!(status.stale_files.contains(&paths.metadata_path));
    assert!(status.stale_files.contains(&paths.socket_path));
}

#[cfg(unix)]
#[tokio::test]
async fn serve_preflight_cleans_dead_pid_and_leftover_socket_state() {
    let config = test_config();
    let paths = daemon_paths(&config);
    let pid = dead_pid();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.pid_path, format!("{pid}\n")).unwrap();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    drop(listener);

    ensure_serve_preflight(&config).await.unwrap();

    assert!(!paths.pid_path.exists());
    assert!(!paths.metadata_path.exists());
    assert!(!paths.socket_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn startup_stability_retries_transient_occupied_socket_probe_failure() {
    let future_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    assert!(should_retry_startup_stability_probe(true, future_deadline));

    let expired_deadline = tokio::time::Instant::now() - std::time::Duration::from_millis(1);
    assert!(!should_retry_startup_stability_probe(
        true,
        expired_deadline
    ));

    let future_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    assert!(!should_retry_startup_stability_probe(
        false,
        future_deadline
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn startup_stability_succeeds_when_occupied_socket_probe_crosses_deadline() {
    let config = test_config();
    let mut child = Command::new("sleep").arg("5").spawn().unwrap();
    let child_pid = child.id();

    let result = wait_for_startup_stability_with_probe(
        &config,
        &mut child,
        child_pid,
        "expected-fingerprint",
        || async {
            ProbeRuntime::Stopped {
                occupied_socket: true,
            }
        },
    )
    .await;

    let _ = child.kill();
    let _ = child.wait();
    result.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_stop_refuses_foreign_socket_cleanup() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    let _listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    let err = daemon_stop(&config).await.unwrap_err().to_string();
    assert!(err.contains("refusing to clean it up"));
    assert!(config.socket_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_stop_treats_missing_pid_process_as_stale_state() {
    let config = test_config();
    let paths = daemon_paths(&config);
    let pid = dead_pid();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.pid_path, format!("{pid}\n")).unwrap();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let stopped = daemon_stop(&config).await.unwrap();
    assert_eq!(stopped.action, crate::daemon::DaemonLifecycleAction::Stop);
    assert_eq!(stopped.status.state, DaemonLifecycleState::Stopped);
    assert!(!paths.pid_path.exists());
    assert!(!paths.metadata_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_stop_uses_recorded_runtime_http_addr_when_socket_is_missing() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut config = test_config();
    config.http_addr = "127.0.0.1:9".into();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metadata_http = listener.local_addr().unwrap().to_string();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: metadata_http.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("POST /control/runtime/shutdown "));
        child.kill().unwrap();
        child.wait().unwrap();
        let body = serde_json::json!({
            "ok": true,
            "pid": pid,
            "home_dir": metadata.home_dir,
            "shutdown_requested": true
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    });

    let stopped = daemon_stop(&config).await.unwrap();
    server.await.unwrap();

    assert_eq!(stopped.action, crate::daemon::DaemonLifecycleAction::Stop);
    assert_eq!(stopped.status.state, DaemonLifecycleState::Stopped);
    assert!(!paths.metadata_path.exists());
    assert!(!paths.pid_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_stop_surfaces_incompatible_status_probe_guidance() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    let socket_path = config.socket_path.clone();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"ok\": invalid}";
            stream.write_all(response).await.unwrap();
            stream.flush().await.unwrap();
        }
    });

    let err = daemon_stop(&config).await.unwrap_err().to_string();
    server.await.unwrap();

    assert!(err
        .contains("cannot stop runtime: the control socket exists, but the status probe failed."));
    assert!(err.contains("stale socket with no runtime currently serving it"));
    assert!(err.contains("Probe error:"));
    assert!(err.contains(
        "failed to decode response body for GET /control/runtime/readiness over unix socket"
    ));
    assert!(socket_path.exists());
}

/// Regression: when both TERM and KILL return PermissionDenied (EPERM),
/// `daemon_stop` must return an error and must NOT clean up state files,
/// because the daemon process may still be running.
///
/// Uses PID 1 (init) as a target — non-root users always get EPERM when
/// signaling PID 1, giving a deterministic PermissionDenied path without
/// mocks or extra seams.
#[cfg(unix)]
#[tokio::test]
async fn daemon_stop_errors_on_permission_denied_signals() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    // PID 1 (init/systemd) — always returns EPERM for non-root.
    let pid: u32 = 1;
    fs::write(&paths.pid_path, format!("{pid}\n")).unwrap();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let err = daemon_stop(&config).await.unwrap_err().to_string();
    assert!(
        err.contains("permission denied"),
        "expected permission denied error, got: {err}"
    );
    // State files must NOT be cleaned up when stop fails due to permission denied.
    assert!(
        paths.pid_path.exists(),
        "PID file should still exist after permission-denied stop failure"
    );
    assert!(
        paths.metadata_path.exists(),
        "metadata file should still exist after permission-denied stop failure"
    );
}
