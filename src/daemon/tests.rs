use super::lifecycle::{
    daemon_status_with_probe_timeout, effective_config_mismatch_summary, lifecycle_lock,
    probe_runtime, probe_runtime_with_timeout, runtime_status_matches_metadata,
    set_prepare_runtime_before_server_hook, should_retry_startup_stability_probe,
    validate_incompatible_runtime_identity_with_executable, wait_for_startup_stability_with_probe,
    web_url, ProbeRuntime,
};
use super::logging::DaemonLogHealth;
use super::state::{
    persist_last_runtime_failure, DAEMON_LOG_TAIL_LINE_CHAR_LIMIT, DAEMON_LOG_TAIL_READ_BYTE_LIMIT,
};
use super::{
    clear_persisted_daemon_lifecycle_failures, config_fingerprint, daemon_log_hint, daemon_logs,
    daemon_paths, daemon_prepare_update, daemon_start, daemon_status, daemon_stop,
    ensure_serve_preflight, load_daemon_desired_running, load_last_runtime_failure,
    persist_daemon_desired_running, persist_daemon_lifecycle_failure,
    prepare_runtime_before_server, runtime_activity_summary, DaemonLifecycleAction,
    DaemonLifecycleState, RuntimeActivityState, RuntimeConfigSurface, RuntimeControlAuthMode,
    RuntimeServiceMetadata, RuntimeStartupSurface, RuntimeStatusResponse,
};
use crate::config::{provider_registry_for_tests, AppConfig, ProviderId};
use crate::{
    host::RuntimeHost,
    provider::StubProvider,
    runtime_db::{RuntimeDb, RuntimeDbLock},
    storage::AppStorage,
    types::{
        AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentState,
        AgentVisibility, AuthorityClass, CommandTaskSpec, RuntimeFailurePhase,
        RuntimeFailureSummary,
    },
};
use chrono::Utc;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tempfile::tempdir;

static PREPARE_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct PrepareHookGuard;

impl Drop for PrepareHookGuard {
    fn drop(&mut self) {
        set_prepare_runtime_before_server_hook(None);
    }
}

fn set_prepare_hook(
    hook: impl Fn(&AppConfig) -> anyhow::Result<()> + Send + Sync + 'static,
) -> PrepareHookGuard {
    set_prepare_runtime_before_server_hook(Some(Box::new(hook)));
    PrepareHookGuard
}

fn serve_profile_fixture() -> super::DaemonServeProfile {
    super::DaemonServeProfile {
        schema_version: 1,
        access: super::ServeProfileAccess::Local,
        host: None,
        listen: Some("127.0.0.1:8787".into()),
        port: None,
        advertise: None,
        cloudflare_token_file: None,
        desktop_integration: false,
        web_dist: None,
        authentication: super::ServeProfileAuthentication::ExternalRequired,
        control_auth_mode: RuntimeControlAuthMode::Required,
    }
}

#[test]
fn serve_profile_roundtrip_is_independent_of_runtime_cleanup() {
    let config = test_config();
    let profile = serve_profile_fixture();
    assert_eq!(super::load_serve_profile(&config).unwrap(), None);
    super::persist_serve_profile(&config, &profile).unwrap();
    super::cleanup_daemon_state(&config).unwrap();
    assert_eq!(super::load_serve_profile(&config).unwrap(), Some(profile));
    assert!(!super::serve_profile_path(&config).starts_with(config.run_dir()));
}

#[test]
fn serve_profile_v1_without_cloudflare_field_remains_compatible() {
    let mut json = serde_json::to_value(serve_profile_fixture()).unwrap();
    json.as_object_mut()
        .unwrap()
        .remove("cloudflare_token_file");
    let profile: super::DaemonServeProfile = serde_json::from_value(json).unwrap();
    profile.validate().unwrap();
    assert!(profile.cloudflare_token_file.is_none());
}

#[test]
fn serve_profile_cloudflare_cannot_disable_auth_or_store_relative_credentials() {
    let mut profile = serve_profile_fixture();
    profile.access = super::ServeProfileAccess::Cloudflare;
    profile.cloudflare_token_file = Some(std::env::temp_dir().join("connector.token"));
    profile.advertise = Some("https://holon.example.com".into());
    profile.validate().unwrap();
    profile.control_auth_mode = RuntimeControlAuthMode::Disabled;
    assert!(profile.validate().is_err());
    profile.control_auth_mode = RuntimeControlAuthMode::Required;
    profile.cloudflare_token_file = Some("connector.token".into());
    assert!(profile.validate().is_err());
}

#[test]
fn serve_profile_roundtrip_preserves_ephemeral_port_intent() {
    let config = test_config();
    let mut profile = serve_profile_fixture();
    profile.listen = Some("127.0.0.1:0".into());
    super::persist_serve_profile(&config, &profile).unwrap();
    assert_eq!(super::load_serve_profile(&config).unwrap(), Some(profile));
}

#[cfg(unix)]
#[test]
fn serve_profile_atomic_replacement_has_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let config = test_config();
    let mut profile = serve_profile_fixture();
    super::persist_serve_profile(&config, &profile).unwrap();
    profile.desktop_integration = true;
    super::persist_serve_profile(&config, &profile).unwrap();
    let path = super::serve_profile_path(&config);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    assert_eq!(super::load_serve_profile(&config).unwrap(), Some(profile));
}

#[cfg(unix)]
#[test]
fn serve_profile_rejects_symlink_file_and_directory_without_touching_target() {
    use std::os::unix::fs::symlink;
    let config = test_config();
    let target = tempdir().unwrap();
    fs::create_dir_all(config.home_dir.join("daemon")).unwrap();
    let sentinel = target.path().join("sentinel");
    fs::write(&sentinel, b"untouched").unwrap();
    symlink(&sentinel, super::serve_profile_path(&config)).unwrap();
    assert!(super::load_serve_profile(&config).is_err());
    assert!(super::persist_serve_profile(&config, &serve_profile_fixture()).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
    fs::remove_file(super::serve_profile_path(&config)).unwrap();
    fs::remove_dir(config.home_dir.join("daemon")).unwrap();
    symlink(target.path(), config.home_dir.join("daemon")).unwrap();
    assert!(super::load_serve_profile(&config).is_err());
    assert!(super::persist_serve_profile(&config, &serve_profile_fixture()).is_err());
    assert_eq!(fs::read_dir(target.path()).unwrap().count(), 1);
}

#[test]
fn serve_profile_rejects_invalid_versions_shapes_and_relative_paths() {
    let config = test_config();
    let mut profile = serve_profile_fixture();
    profile.schema_version = 99;
    assert!(super::persist_serve_profile(&config, &profile).is_err());
    profile.schema_version = 1;
    profile.port = Some(7878);
    assert!(profile.validate().is_err());
    profile.port = None;
    profile.authentication = super::ServeProfileAuthentication::TokenFile {
        path: "relative.token".into(),
    };
    assert!(profile.validate().is_err());
    super::persist_serve_profile(&config, &serve_profile_fixture()).unwrap();
    fs::write(super::serve_profile_path(&config), b"{broken").unwrap();
    assert!(super::load_serve_profile(&config).is_err());
}

#[tokio::test]
async fn serve_profile_status_reads_stopped_intent_and_rejects_corruption() {
    let config = test_config();
    let profile = serve_profile_fixture();
    super::persist_serve_profile(&config, &profile).unwrap();
    let status = daemon_status(&config).await.unwrap();
    assert_eq!(
        status.desired_serve,
        Some(super::DaemonDesiredServe::Profile {
            profile: profile.clone()
        })
    );
    let encoded = serde_json::to_value(&status).unwrap();
    assert_eq!(encoded["desired_serve"]["source"], "profile");
    assert_eq!(encoded["desired_serve"]["listen"], profile.listen.unwrap());
    assert!(encoded.get("desired_serve_source").is_none());
    fs::write(super::serve_profile_path(&config), b"null").unwrap();
    assert!(daemon_status(&config).await.is_err());
}

#[tokio::test]
async fn serve_profile_status_distinguishes_defaults_legacy_and_unknown() {
    let config = test_config();
    let status = daemon_status(&config).await.unwrap();
    assert_eq!(
        serde_json::to_value(status).unwrap()["desired_serve"]["source"],
        "defaults"
    );
    let metadata = replacement_metadata(&config, 999_999, std::env::current_exe().unwrap());
    fs::create_dir_all(config.run_dir()).unwrap();
    let path = config.run_dir().join("daemon.json");
    fs::write(&path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let status = daemon_status(&config).await.unwrap();
    let encoded = serde_json::to_value(status).unwrap();
    assert_eq!(encoded["desired_serve"]["source"], "legacy");
    assert_eq!(encoded["desired_serve"]["listen"], config.http_addr);
    fs::write(&path, b"{broken").unwrap();
    let status = daemon_status(&config).await.unwrap();
    assert!(status.desired_serve.is_none());
    assert!(!super::serve_profile_path(&config).exists());
}

#[tokio::test]
async fn serve_profile_failed_resolution_keeps_profile_and_running_intent() {
    let config = test_config();
    let profile = serve_profile_fixture();
    super::persist_serve_profile(&config, &profile).unwrap();
    persist_daemon_desired_running(&config, false).unwrap();
    let error = super::daemon_launch_with_profile(
        &config,
        true,
        false,
        None,
        |_, _| anyhow::bail!("invalid requested token file"),
        |_, _| unreachable!("failed resolution must not inspect running serve options"),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("invalid requested"));
    assert_eq!(super::load_serve_profile(&config).unwrap(), Some(profile));
    assert_eq!(load_daemon_desired_running(&config).unwrap(), Some(false));
}

#[cfg(unix)]
#[tokio::test]
async fn serve_profile_foreign_socket_does_not_accept_new_intent() {
    let config = test_config();
    let profile = serve_profile_fixture();
    super::persist_serve_profile(&config, &profile).unwrap();
    persist_daemon_desired_running(&config, false).unwrap();
    fs::create_dir_all(config.run_dir()).unwrap();
    let _listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    let result = super::daemon_launch_with_profile(
        &config,
        true,
        false,
        None,
        |_, _| {
            let mut updated = profile.clone();
            updated.listen = Some("127.0.0.1:9999".into());
            Ok(super::ResolvedDaemonServePlan {
                config: config.clone(),
                profile: updated,
                serve_args: vec![],
                control_token_env: None,
                startup_summary: String::new(),
            })
        },
        |_, _| unreachable!("foreign sockets must not be adopted"),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(super::load_serve_profile(&config).unwrap(), Some(profile));
    assert_eq!(load_daemon_desired_running(&config).unwrap(), Some(false));
    assert!(config.socket_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn serve_profile_foreground_retains_lock_until_runtime_registration() {
    let config = test_config();
    let profile = serve_profile_fixture();
    let (plan, registration_lock) = super::prepare_serve_with_profile(&config, false, |_, _| {
        Ok(super::ResolvedDaemonServePlan {
            config: config.clone(),
            profile: profile.clone(),
            serve_args: vec![],
            control_token_env: None,
            startup_summary: String::new(),
        })
    })
    .await
    .unwrap();

    let lock_path = daemon_paths(&config).lifecycle_lock_path;
    assert!(RuntimeDbLock::try_lock(&lock_path).is_err());
    assert!(!config.socket_path.exists());
    assert!(!daemon_paths(&config).metadata_path.exists());

    let _listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    let service = super::RuntimeServiceHandle::new_starting(&plan.config).unwrap();
    service.write_state_files(&plan.config).unwrap();
    assert!(RuntimeDbLock::try_lock(&lock_path).is_err());
    drop(registration_lock);
    let next_lock = RuntimeDbLock::try_lock(&lock_path).unwrap();
    assert!(daemon_paths(&config).metadata_path.exists());
    assert_eq!(super::load_serve_profile(&config).unwrap(), Some(profile));
    drop(next_lock);
}

fn test_config() -> AppConfig {
    let home = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    AppConfig {
        default_agent_id: "default".into(),
        http_addr: "127.0.0.1:0".into(),
        callback_base_url: "http://127.0.0.1:0".into(),
        user_home_dir: None,
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
        auth: Default::default(),
        api_cors: Default::default(),
        api_projection: Default::default(),
        local_apps: Default::default(),
        config_file_path: home.path().join("config.json"),
        stored_config: Default::default(),
        default_model: crate::config::ModelRouteRef::new(
            ProviderId::anthropic(),
            crate::config::ProviderEndpointId::default_endpoint(),
            "claude-sonnet-4-6",
        ),
        fallback_models: vec![],
        vision_model: None,
        image_generation_model: None,
        vision_candidate_models: Vec::new(),
        runtime_max_output_tokens: 8192,
        default_tool_output_tokens: crate::tool::helpers::DEFAULT_TOOL_OUTPUT_TOKENS as u32,
        max_tool_output_tokens: crate::tool::helpers::MAX_TOOL_OUTPUT_TOKENS as u32,
        command_task_output_retention_bytes: 8 * 1024 * 1024,
        command_task_output_quota_bytes: 64 * 1024 * 1024,
        command_task_min_free_disk_bytes: 512 * 1024 * 1024,
        command_task_min_free_disk_percent: 5,
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
    assert_eq!(
        paths.lifecycle_lock_path,
        config.run_dir().join("lifecycle.lock")
    );
    assert_eq!(
        paths.desired_state_path,
        config.run_dir().join("desired_state.json")
    );
}

fn replacement_metadata(
    config: &AppConfig,
    pid: u32,
    executable_path: PathBuf,
) -> RuntimeServiceMetadata {
    RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(config).unwrap(),
        product_version: "incompatible-build".into(),
        control_protocol_version: 0,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path,
        serve_args: Vec::new(),
        control_token_env_configured: false,
    }
}

#[test]
fn incompatible_runtime_identity_rejects_home_or_socket_mismatch() {
    let config = test_config();
    let mut metadata = replacement_metadata(&config, 42, PathBuf::new());
    metadata.home_dir = config.home_dir.join("other");

    let error = validate_incompatible_runtime_identity_with_executable(
        &config,
        &metadata,
        PathBuf::from("/tmp/holon").as_path(),
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("recorded home or control socket"));
}

#[test]
fn incompatible_runtime_identity_rejects_pid_mismatch() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(daemon_paths(&config).pid_path, "41\n").unwrap();
    let metadata = replacement_metadata(&config, 42, PathBuf::new());

    let error = validate_incompatible_runtime_identity_with_executable(
        &config,
        &metadata,
        PathBuf::from("/tmp/holon").as_path(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("records PID 41"));
    assert!(error.to_string().contains("metadata records PID 42"));
}

#[test]
fn incompatible_runtime_identity_rejects_wrong_executable_name() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(daemon_paths(&config).pid_path, "42\n").unwrap();
    let metadata = replacement_metadata(&config, 42, PathBuf::new());

    let error = validate_incompatible_runtime_identity_with_executable(
        &config,
        &metadata,
        PathBuf::from("/tmp/not-holon").as_path(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("its executable is"));
}

#[test]
fn incompatible_runtime_identity_rejects_different_executable_path() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(daemon_paths(&config).pid_path, "42\n").unwrap();
    let executable_dir = tempdir().unwrap();
    let recorded_dir = tempdir().unwrap();
    let actual_executable = executable_dir.path().join("holon");
    let recorded_executable = recorded_dir.path().join("holon");
    fs::write(&actual_executable, b"actual").unwrap();
    fs::write(&recorded_executable, b"recorded").unwrap();
    let metadata = replacement_metadata(&config, 42, recorded_executable);

    let error = validate_incompatible_runtime_identity_with_executable(
        &config,
        &metadata,
        &actual_executable,
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("does not match recorded executable"));
}

#[test]
fn incompatible_runtime_identity_accepts_matching_identity() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(daemon_paths(&config).pid_path, "42\n").unwrap();
    let executable_dir = tempdir().unwrap();
    let actual_executable = executable_dir.path().join("holon");
    fs::write(&actual_executable, b"holon").unwrap();
    let metadata = replacement_metadata(&config, 42, actual_executable.clone());

    validate_incompatible_runtime_identity_with_executable(&config, &metadata, &actual_executable)
        .unwrap();
}

#[test]
fn daemon_web_url_uses_a_connectable_loopback_address() {
    assert_eq!(web_url("0.0.0.0:7878"), "http://127.0.0.1:7878");
    assert_eq!(web_url("[::]:7878"), "http://[::1]:7878");
    assert_eq!(web_url("127.0.0.1:7878"), "http://127.0.0.1:7878");
    assert_eq!(web_url("https://holon.example"), "https://holon.example");
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
    };
    let encoded = serde_json::to_vec(&metadata).unwrap();
    let decoded: RuntimeServiceMetadata = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.pid, 42);
    assert_eq!(decoded.home_dir, config.home_dir);
}

#[test]
fn runtime_service_metadata_defaults_new_lifecycle_fields_for_old_records() {
    let decoded: RuntimeServiceMetadata = serde_json::from_value(serde_json::json!({
        "pid": 42,
        "home_dir": "/tmp/holon",
        "socket_path": "/tmp/holon/run/control.sock",
        "http_addr": "127.0.0.1:7878",
        "started_at": "2026-08-28T00:00:00Z",
        "config_fingerprint": "old"
    }))
    .unwrap();

    assert_eq!(decoded.product_version, "");
    assert_eq!(decoded.control_protocol_version, 0);
    assert_eq!(
        decoded.lifecycle_owner,
        crate::daemon::DaemonLifecycleOwner::Standalone
    );
    assert!(decoded.executable_path.as_os_str().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_status_reports_old_live_metadata_as_version_mismatch() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.pid_path, format!("{}\n", std::process::id())).unwrap();
    fs::write(
        &paths.metadata_path,
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(),
            "home_dir": config.home_dir,
            "socket_path": config.socket_path,
            "http_addr": config.http_addr,
            "started_at": "2026-08-28T00:00:00Z",
            "config_fingerprint": config_fingerprint(&config).unwrap()
        }))
        .unwrap(),
    )
    .unwrap();

    let status = daemon_status(&config).await.unwrap();
    assert_eq!(status.state, DaemonLifecycleState::VersionMismatch);
    assert_eq!(status.control_protocol_version, None);
    assert!(status.message.contains("control protocol version 0"));
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_status_accepts_different_product_build_with_compatible_protocol() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.pid_path, format!("{}\n", std::process::id())).unwrap();
    fs::write(
        &paths.metadata_path,
        serde_json::to_vec(&RuntimeServiceMetadata {
            pid: std::process::id(),
            home_dir: config.home_dir.clone(),
            socket_path: config.socket_path.clone(),
            http_addr: config.http_addr.clone(),
            started_at: Utc::now(),
            config_fingerprint: config_fingerprint(&config).unwrap(),
            product_version: "0.32.0 (old-build)".into(),
            control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
            lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
            executable_path: std::env::current_exe().unwrap(),
            serve_args: Vec::new(),
            control_token_env_configured: false,
        })
        .unwrap(),
    )
    .unwrap();

    let status = daemon_status(&config).await.unwrap();
    assert_eq!(status.state, DaemonLifecycleState::Degraded);
    assert_eq!(
        status.product_version.as_deref(),
        Some("0.32.0 (old-build)")
    );
    assert_eq!(
        status.control_protocol_version,
        Some(crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION)
    );
    assert!(!status.message.contains("product version"));
}

#[test]
fn daemon_desired_state_round_trips_without_affecting_runtime_files() {
    let config = test_config();
    assert_eq!(load_daemon_desired_running(&config).unwrap(), None);

    persist_daemon_desired_running(&config, true).unwrap();
    assert_eq!(load_daemon_desired_running(&config).unwrap(), Some(true));

    persist_daemon_desired_running(&config, false).unwrap();
    assert_eq!(load_daemon_desired_running(&config).unwrap(), Some(false));
    assert!(!daemon_paths(&config).metadata_path.exists());
}

#[tokio::test]
async fn daemon_lifecycle_lock_rejects_a_second_holder() {
    let config = test_config();
    let _lock = lifecycle_lock(&config).await.unwrap();
    let second = RuntimeDbLock::try_lock(daemon_paths(&config).lifecycle_lock_path);
    assert!(second.is_err());
}

#[tokio::test]
async fn daemon_prepare_update_stops_without_changing_desired_state() {
    let config = test_config();
    persist_daemon_desired_running(&config, true).unwrap();

    let result = daemon_prepare_update(&config).await.unwrap();

    assert_eq!(result.action, DaemonLifecycleAction::PrepareUpdate);
    assert_eq!(result.status.state, DaemonLifecycleState::Stopped);
    assert!(result.status.desired_running);
    assert_eq!(load_daemon_desired_running(&config).unwrap(), Some(true));
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
    assert_eq!(view.log_health, None);
    assert_eq!(
        view.tail,
        vec!["third line".to_string(), "fourth line".to_string()]
    );
    let json = serde_json::to_value(&view).unwrap();
    assert!(json.get("log_health").is_none());
    let decoded: super::DaemonLogsView = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, view);
}

#[test]
fn daemon_logs_surface_degraded_log_health_even_when_tail_is_omitted() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.log_path, "last line\n").unwrap();
    let health = DaemonLogHealth {
        degraded: true,
        buffered_bytes: 4096,
        dropped_bytes: 8192,
        last_error: Some("no space left on device".into()),
        ..DaemonLogHealth::default()
    };
    fs::write(
        paths.log_path.with_file_name("daemon.log.status.json"),
        serde_json::to_vec(&health).unwrap(),
    )
    .unwrap();

    for tail_lines in [0, 1] {
        let view = daemon_logs(&config, tail_lines).unwrap();
        assert!(view.ok);
        assert_eq!(view.log_health, Some(health.clone()));
        assert!(view.message.contains("daemon logging degraded"));
        assert!(view.message.contains("dropped_bytes=8192"));
        assert!(view.message.contains("buffered_bytes=4096"));
        assert_eq!(view.tail.len(), tail_lines);
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["log_health"], serde_json::to_value(&health).unwrap());
    }
}

#[test]
fn daemon_logs_surface_healthy_log_health_without_degraded_message() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    let health = DaemonLogHealth {
        dropped_bytes: 512,
        ..DaemonLogHealth::default()
    };
    fs::write(
        paths.log_path.with_file_name("daemon.log.status.json"),
        serde_json::to_vec(&health).unwrap(),
    )
    .unwrap();

    let view = daemon_logs(&config, 0).unwrap();
    assert!(view.ok);
    assert_eq!(view.log_health, Some(health));
    assert_eq!(view.message, "daemon log tail omitted (--tail 0)");
}

#[test]
fn daemon_logs_preserve_tail_when_log_health_is_unavailable() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.log_path, "last line\n").unwrap();
    let health_path = paths.log_path.with_file_name("daemon.log.status.json");
    fs::write(&health_path, b"{\"degraded\":").unwrap();

    for unreadable in [false, true] {
        if unreadable {
            fs::remove_file(&health_path).unwrap();
            fs::create_dir(&health_path).unwrap();
        }
        for tail_lines in [0, 1] {
            let view = daemon_logs(&config, tail_lines).unwrap();
            assert!(view.ok);
            assert_eq!(view.log_health, None);
            assert!(view.message.contains("daemon log health unavailable"));
            assert_eq!(view.tail.len(), tail_lines);
            if tail_lines > 0 {
                assert_eq!(view.tail, ["last line"]);
            }
            assert!(serde_json::to_value(&view)
                .unwrap()
                .get("log_health")
                .is_none());
        }
    }
}

#[test]
fn daemon_logs_tail_zero_omits_log_tail() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.log_path, "first line\nsecond line\n").unwrap();
    fs::create_dir(paths.log_path.with_file_name("daemon.log.1")).unwrap();

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

#[test]
fn daemon_logs_tail_reads_rotated_segments_in_order() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    for (name, text) in [
        ("daemon.log.4", "not retained\n"),
        ("daemon.log.3", "first\nsecond\n"),
        ("daemon.log.2", "third\nfourth\n"),
        ("daemon.log.1", "fifth\nsixth\n"),
        ("daemon.log", "seventh\neighth\n"),
    ] {
        fs::write(paths.log_path.with_file_name(name), text).unwrap();
    }

    assert_eq!(
        daemon_logs(&config, 20).unwrap().tail,
        ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth"]
    );
    assert_eq!(
        daemon_logs(&config, 5).unwrap().tail,
        ["fourth", "fifth", "sixth", "seventh", "eighth"]
    );
}

#[test]
fn daemon_logs_tail_joins_lines_and_crlf_across_segments() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    for (name, text) in [
        ("daemon.log.3", "older\r"),
        ("daemon.log.2", "\njoined"),
        ("daemon.log.1", " across"),
        ("daemon.log", " segments\nlast"),
    ] {
        fs::write(paths.log_path.with_file_name(name), text).unwrap();
    }

    assert_eq!(
        daemon_logs(&config, 3).unwrap().tail,
        ["older", "joined across segments", "last"]
    );
    assert_eq!(daemon_logs(&config, 1).unwrap().tail, ["last"]);
}

#[test]
fn daemon_logs_tail_reads_old_segments_without_current_or_intermediate_files() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(paths.log_path.with_file_name("daemon.log.3"), "oldest\n").unwrap();
    fs::write(paths.log_path.with_file_name("daemon.log.1"), "newest\n").unwrap();

    assert_eq!(daemon_logs(&config, 10).unwrap().tail, ["oldest", "newest"]);
    fs::write(&paths.log_path, "").unwrap();
    fs::write(paths.log_path.with_file_name("daemon.log.2"), "").unwrap();
    assert_eq!(daemon_logs(&config, 10).unwrap().tail, ["oldest", "newest"]);
}

#[test]
fn daemon_logs_tail_shares_byte_budget_across_segments() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    let quarter_budget = DAEMON_LOG_TAIL_READ_BYTE_LIMIT / 4;
    let suffix = "tail-marker\n";
    fs::write(
        paths.log_path.with_file_name("daemon.log.3"),
        "outside-budget-older\n",
    )
    .unwrap();
    fs::write(
        paths.log_path.with_file_name("daemon.log.2"),
        format!("outside-budget\n{}", "x".repeat(quarter_budget * 2)),
    )
    .unwrap();
    fs::write(
        paths.log_path.with_file_name("daemon.log.1"),
        "y".repeat(quarter_budget),
    )
    .unwrap();
    fs::write(
        &paths.log_path,
        format!("{}{suffix}", "z".repeat(quarter_budget - suffix.len())),
    )
    .unwrap();

    let view = daemon_logs(&config, 100).unwrap();
    assert_eq!(view.tail.len(), 1);
    assert_eq!(
        view.tail[0].chars().count(),
        DAEMON_LOG_TAIL_LINE_CHAR_LIMIT
    );
    assert!(view.tail[0].starts_with("..."));
    assert!(view.tail[0].ends_with("tail-marker"));
    assert!(!view.tail[0].contains("outside-budget"));
}

#[test]
fn daemon_logs_tail_decodes_utf8_after_joining_segments() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    for (name, bytes) in [
        ("daemon.log.3", &b"older\n\xe4"[..]),
        ("daemon.log.2", &b"\xb8\xad\xe6"[..]),
        ("daemon.log.1", &b"\x96\x87\xf0\x9f"[..]),
        ("daemon.log", &b"\x98\x80 tail\nlast\n"[..]),
    ] {
        fs::write(paths.log_path.with_file_name(name), bytes).unwrap();
    }

    assert_eq!(
        daemon_logs(&config, 3).unwrap().tail,
        ["older", "中文😀 tail", "last"]
    );
}

#[test]
fn daemon_logs_tail_preserves_utf8_across_read_chunks() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    let line = format!("{} tail", "界".repeat(5_000));
    fs::write(&paths.log_path, format!("older\n{line}\n")).unwrap();

    assert_eq!(daemon_logs(&config, 1).unwrap().tail, [line]);
}

#[test]
fn daemon_logs_tail_truncates_multibyte_lines_at_character_boundaries() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(
        paths.log_path.with_file_name("daemon.log.1"),
        format!(
            "outside-budget\n{}",
            "界".repeat(DAEMON_LOG_TAIL_READ_BYTE_LIMIT)
        ),
    )
    .unwrap();
    fs::write(&paths.log_path, "😀 tail\n").unwrap();

    let tail = daemon_logs(&config, 100).unwrap().tail;
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].chars().count(), DAEMON_LOG_TAIL_LINE_CHAR_LIMIT);
    assert!(!tail[0].contains('\u{fffd}'));
    assert_eq!(
        tail[0],
        format!(
            "...{}😀 tail",
            "界".repeat(DAEMON_LOG_TAIL_LINE_CHAR_LIMIT - 9)
        )
    );
}

#[cfg(unix)]
#[test]
fn daemon_logs_tail_does_not_read_a_rotated_inode_twice() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    fs::write(&paths.log_path, "newest\n").unwrap();
    fs::hard_link(
        &paths.log_path,
        paths.log_path.with_file_name("daemon.log.1"),
    )
    .unwrap();
    fs::write(paths.log_path.with_file_name("daemon.log.2"), "older\n").unwrap();

    assert_eq!(daemon_logs(&config, 10).unwrap().tail, ["older", "newest"]);
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

#[tokio::test]
async fn daemon_status_surfaces_db_only_public_agent_runtime_failure_when_stopped() {
    let config = test_config();
    let runtime_db =
        RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
            .unwrap();
    let host_storage =
        AppStorage::new_global(config.home_dir.join("host"), runtime_db.clone()).unwrap();
    let identity = AgentIdentityRecord::new(
        "public-agent",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    host_storage.append_agent_identity(&identity).unwrap();
    let failure = RuntimeFailureSummary {
        occurred_at: Utc::now(),
        summary: "public runtime failed".into(),
        phase: RuntimeFailurePhase::RuntimeTurn,
        detail_hint: Some("runtime artifact".into()),
        failure_artifact: None,
    };
    let mut agent = AgentState::new("public-agent");
    agent.last_runtime_failure = Some(failure.clone());
    runtime_db
        .agent_states()
        .upsert_for_fixture(&agent)
        .unwrap();

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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        activity: None,
        tunnel: None,
        startup_surface: Some(RuntimeStartupSurface {
            user_home_dir: expected.user_home_dir.clone(),
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
        runtime_db_protection: None,
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

#[test]
fn runtime_config_surface_reports_available_search_provider_capabilities() {
    let surface = RuntimeConfigSurface::new(&test_config());

    assert_eq!(
        surface.available_search_provider_kinds.len(),
        crate::web::WebProviderKind::ALL.len()
    );
    let brave = surface
        .available_search_provider_kinds
        .iter()
        .find(|provider| provider.kind == "brave")
        .expect("brave provider kind should be reported");
    assert_eq!(
        brave.capabilities.auth,
        crate::web::WebProviderAuthClass::ApiKey
    );
    assert_eq!(brave.capabilities.default_priority, 80);

    let native = surface
        .available_search_provider_kinds
        .iter()
        .find(|provider| provider.kind == "open_ai_native")
        .expect("native provider kind should be reported");
    assert_eq!(
        native.capabilities.status,
        crate::web::WebProviderSupportStatus::NativeOnly
    );
}

#[test]
fn runtime_config_surface_reports_credential_free_providers_as_ready() {
    let mut config = test_config();
    let ollama_id = crate::config::ProviderId::parse("ollama").unwrap();
    config.providers.insert(
        ollama_id.clone(),
        crate::config::ProviderRuntimeConfig {
            id: ollama_id.clone(),
            route_provider: ollama_id,
            route_endpoint: crate::config::ProviderEndpointId::default_endpoint(),
            transport: crate::config::ProviderTransportKind::AnthropicMessages,
            base_url: "http://127.0.0.1:11434".into(),
            auth: crate::config::ProviderAuthConfig {
                source: crate::config::CredentialSource::None,
                kind: crate::config::CredentialKind::None,
                env: None,
                profile: None,
                external: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );

    let surface = RuntimeConfigSurface::new(&config);
    let ollama = surface
        .providers
        .iter()
        .find(|provider| provider.id == "ollama")
        .expect("ollama provider should be reported");
    assert_eq!(ollama.credential_kind, "none");
    assert!(!ollama.api_key_supported);
    assert!(ollama.credential_configured);
}

#[test]
fn runtime_config_surface_reports_decision_provider_settings() {
    let mut config = test_config();
    config.stored_config.decision.enabled = Some(true);
    config.stored_config.decision.model = Some("typesafe@default/jev-latest".into());

    let surface = RuntimeConfigSurface::new(&config);

    assert!(surface.decision.enabled);
    assert_eq!(
        surface.decision.model.as_deref(),
        Some("typesafe@default/jev-latest")
    );
    assert_eq!(surface.decision.provider.as_deref(), Some("typesafe"));
    assert_eq!(surface.decision.endpoint.as_deref(), Some("default"));
    assert_eq!(surface.decision.route_provider.as_deref(), Some("typesafe"));
    assert_eq!(
        surface.decision.transport.as_deref(),
        Some("ai_evaluation_model")
    );
    assert!(surface.decision.decision_capable);
    assert!(!surface.decision.credential_configured);
    assert!(!surface.decision.available);
    assert_eq!(
        surface.decision.unavailable_reason.as_deref(),
        Some("credential_missing")
    );
    assert_eq!(surface.decision.protocol.as_deref(), Some("jev"));
}

#[test]
fn runtime_config_surface_defaults_decision_provider_to_disabled() {
    let surface = RuntimeConfigSurface::new(&test_config());

    assert!(!surface.decision.enabled);
    assert_eq!(surface.decision.model, None);
    assert!(!surface.decision.local_onnx.enabled);
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
        ProbeRuntime::Unresponsive { details } => {
            panic!("expected stale runtime probe, got unresponsive: {details}")
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn probe_runtime_reports_unresponsive_when_readiness_times_out() {
    let config = test_config();
    fs::create_dir_all(config.run_dir()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                drop(stream);
            });
        }
    });

    match probe_runtime_with_timeout(&config, std::time::Duration::from_millis(50)).await {
        ProbeRuntime::Unresponsive { details } => {
            assert!(details.contains("unix readiness probe timed out"));
        }
        other => panic!("expected unresponsive runtime probe, got {:?}", other),
    }

    let status = daemon_status_with_probe_timeout(&config, std::time::Duration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(status.state, DaemonLifecycleState::Unresponsive);
    assert!(!status.healthy);
    assert!(!status.control_connectivity);
    assert!(status.message.contains("unix readiness probe timed out"));
    assert!(!status.message.contains("non-Holon process"));
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
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
async fn daemon_start_reusing_live_process_does_not_claim_creation() {
    let config = test_config();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    let pid = std::process::id();
    let metadata = RuntimeServiceMetadata {
        pid,
        home_dir: config.home_dir.clone(),
        socket_path: config.socket_path.clone(),
        http_addr: config.http_addr.clone(),
        started_at: Utc::now(),
        config_fingerprint: config_fingerprint(&config).unwrap(),
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
    };
    fs::write(&paths.pid_path, format!("{pid}\n")).unwrap();
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let result = daemon_start(&config, &[], None).await.unwrap();
    assert_eq!(result.action, DaemonLifecycleAction::Start);
    assert_eq!(result.status.pid, Some(pid));
    assert!(!result.process_created);
    assert_eq!(result.status.message, "runtime is already running");

    let wire = serde_json::to_value(&result).unwrap();
    assert_eq!(wire["process_created"], false);
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        activity: None,
        tunnel: None,
        startup_surface: None,
        runtime_surface: None,
        last_failure: None,
        runtime_db_protection: None,
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();
    drop(listener);

    ensure_serve_preflight(&config).await.unwrap();

    assert!(!paths.pid_path.exists());
    assert!(!paths.metadata_path.exists());
    assert!(!paths.socket_path.exists());
}

#[test]
fn daemon_start_runs_preparation_before_readiness_timeout_window() {
    let _guard = PREPARE_HOOK_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = test_config();
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let _hook_guard = set_prepare_hook(move |_config| {
        hook_calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("test preparation domain failed")
    });

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let err = runtime
        .block_on(daemon_start(&config, &[], None))
        .unwrap_err()
        .to_string();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(err.contains("pre-server runtime preparation failed"));
    assert!(!err.contains("timed out waiting for runtime"));
    assert!(!daemon_paths(&config).log_path.exists());
}

#[test]
fn pre_server_preparation_failure_is_retryable() {
    let _guard = PREPARE_HOOK_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = test_config();
    let attempts = Arc::new(AtomicUsize::new(0));
    let hook_attempts = attempts.clone();
    let _hook_guard = set_prepare_hook(move |_config| {
        let attempt = hook_attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            anyhow::bail!("test storage domain incomplete")
        }
        Ok(())
    });

    let first = prepare_runtime_before_server(&config)
        .unwrap_err()
        .to_string();
    let second = prepare_runtime_before_server(&config);

    assert!(first.contains("pre-server runtime preparation failed"));
    second.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
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
async fn serve_profile_corruption_does_not_relax_foreign_socket_stop_guard() {
    let config = test_config();
    super::persist_serve_profile(&config, &serve_profile_fixture()).unwrap();
    fs::write(super::serve_profile_path(&config), b"{broken").unwrap();
    fs::create_dir_all(config.run_dir()).unwrap();
    let _listener = tokio::net::UnixListener::bind(&config.socket_path).unwrap();

    let error = daemon_stop(&config).await.unwrap_err();
    assert!(error.to_string().contains("refusing to clean it up"));
    assert!(config.socket_path.exists());
    assert_eq!(
        fs::read(super::serve_profile_path(&config)).unwrap(),
        b"{broken"
    );
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let stopped = daemon_stop(&config).await.unwrap();
    assert_eq!(stopped.action, crate::daemon::DaemonLifecycleAction::Stop);
    assert!(!stopped.process_created);
    assert_eq!(stopped.status.state, DaemonLifecycleState::Stopped);
    assert!(!paths.pid_path.exists());
    assert!(!paths.metadata_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_stop_uses_recorded_runtime_http_addr_when_socket_is_missing() {
    assert_shutdown_with_recorded_runtime_addr(false, false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn serve_profile_corruption_does_not_block_running_daemon_stop() {
    assert_shutdown_with_recorded_runtime_addr(false, true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn serve_profile_corruption_does_not_block_running_prepare_update() {
    assert_shutdown_with_recorded_runtime_addr(true, true).await;
}

#[cfg(unix)]
async fn assert_shutdown_with_recorded_runtime_addr(prepare_update: bool, corrupt_profile: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut config = test_config();
    config.http_addr = "127.0.0.1:9".into();
    let paths = daemon_paths(&config);
    fs::create_dir_all(config.run_dir()).unwrap();
    if corrupt_profile {
        super::persist_serve_profile(&config, &serve_profile_fixture()).unwrap();
        fs::write(super::serve_profile_path(&config), b"{broken").unwrap();
        persist_daemon_desired_running(&config, true).unwrap();
        assert!(daemon_status(&config).await.is_err());
    }

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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
    };
    fs::write(&paths.metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();

    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        let read = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("POST /api/control/runtime/shutdown "));
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

    let stopped = if prepare_update {
        daemon_prepare_update(&config).await.unwrap()
    } else {
        daemon_stop(&config).await.unwrap()
    };
    server.await.unwrap();

    assert_eq!(
        stopped.action,
        if prepare_update {
            DaemonLifecycleAction::PrepareUpdate
        } else {
            DaemonLifecycleAction::Stop
        }
    );
    assert_eq!(stopped.status.state, DaemonLifecycleState::Stopped);
    assert!(!paths.metadata_path.exists());
    assert!(!paths.pid_path.exists());
    if corrupt_profile {
        assert_eq!(stopped.status.desired_running, prepare_update);
        assert_eq!(
            load_daemon_desired_running(&config).unwrap(),
            Some(prepare_update)
        );
        assert!(stopped.status.desired_serve.is_none());
        assert_eq!(
            fs::read(super::serve_profile_path(&config)).unwrap(),
            b"{broken"
        );
        assert!(daemon_status(&config).await.is_err());
    }
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
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await.unwrap();
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"ok\": invalid}";
        stream.write_all(response).await.unwrap();
        stream.flush().await.unwrap();
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
        product_version: env!("HOLON_VERSION").into(),
        control_protocol_version: crate::daemon::DAEMON_CONTROL_PROTOCOL_VERSION,
        lifecycle_owner: crate::daemon::DaemonLifecycleOwner::Standalone,
        executable_path: std::env::current_exe().unwrap(),
        serve_args: Vec::new(),
        control_token_env_configured: false,
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
