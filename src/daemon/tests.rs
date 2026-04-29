use super::lifecycle::{probe_runtime, ProbeRuntime};
use super::state::{
    persist_last_runtime_failure, DAEMON_LOG_TAIL_LINE_CHAR_LIMIT, DAEMON_LOG_TAIL_READ_BYTE_LIMIT,
};
use super::{
    clear_persisted_daemon_lifecycle_failures, config_fingerprint, daemon_log_hint, daemon_logs,
    daemon_paths, daemon_status, daemon_stop, load_last_runtime_failure,
    persist_daemon_lifecycle_failure, runtime_activity_summary, DaemonLifecycleState,
    RuntimeActivityState, RuntimeServiceMetadata, RuntimeStatusResponse,
};
use crate::config::{provider_registry_for_tests, AppConfig, ModelRef, ProviderId};
use crate::{
    host::RuntimeHost,
    provider::StubProvider,
    types::{CommandTaskSpec, RuntimeFailurePhase, RuntimeFailureSummary, TrustLevel},
};
use chrono::Utc;
use std::{fs, sync::Arc};
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
        disable_provider_fallback: false,
        tui_alternate_screen: crate::config::AltScreenMode::Auto,
        validated_model_overrides: std::collections::HashMap::new(),
        validated_unknown_model_fallback: None,
        providers: provider_registry_for_tests(None, Some("dummy"), home.path().join(".codex")),
    }
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
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
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
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.pid_path, b"999999\n").unwrap();
    let metadata = RuntimeServiceMetadata {
        pid: 999_999,
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
        "failed to decode response body for GET /control/runtime/status over unix socket"
    ));
    assert!(socket_path.exists());
}
