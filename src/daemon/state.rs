use std::{
    fs,
    io::{ErrorKind, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    config::AppConfig,
    storage::AppStorage,
    types::{AgentRegistryStatus, AgentVisibility, RuntimeFailurePhase, RuntimeFailureSummary},
};

use super::{RuntimeActivitySummary, RuntimeServiceMetadata};

pub(crate) const DAEMON_LOG_TAIL_READ_BYTE_LIMIT: usize = 128 * 1024;
pub(crate) const DAEMON_LOG_TAIL_LINE_CHAR_LIMIT: usize = 8 * 1024;
const DAEMON_LOG_TAIL_READ_CHUNK_SIZE: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLifecycleAction {
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLifecycleState {
    Running,
    Stopped,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatusView {
    pub ok: bool,
    pub state: DaemonLifecycleState,
    pub healthy: bool,
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub http_addr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub control_connectivity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_fingerprint_match: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<RuntimeActivitySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<RuntimeFailureSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_files: Vec<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonLifecycleResult {
    pub ok: bool,
    pub action: DaemonLifecycleAction,
    #[serde(flatten)]
    pub status: DaemonStatusView,
}

#[derive(Debug, Clone)]
pub struct DaemonPaths {
    pub run_dir: PathBuf,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub metadata_path: PathBuf,
    pub log_path: PathBuf,
    pub last_failure_path: PathBuf,
    pub startup_failure_path: PathBuf,
    pub shutdown_failure_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonLogsView {
    pub ok: bool,
    pub log_path: PathBuf,
    pub metadata_path: PathBuf,
    pub last_failure_path: PathBuf,
    pub startup_failure_path: PathBuf,
    pub shutdown_failure_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_metadata: Option<RuntimeServiceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<RuntimeFailureSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_failure: Option<RuntimeFailureSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_failure: Option<RuntimeFailureSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<String>,
    pub message: String,
}

pub fn daemon_paths(config: &AppConfig) -> DaemonPaths {
    let run_dir = config.run_dir();
    DaemonPaths {
        run_dir: run_dir.clone(),
        socket_path: config.socket_path.clone(),
        pid_path: run_dir.join("holon.pid"),
        metadata_path: run_dir.join("daemon.json"),
        log_path: run_dir.join("daemon.log"),
        last_failure_path: run_dir.join("last_failure.json"),
        startup_failure_path: run_dir.join("startup_failure.json"),
        shutdown_failure_path: run_dir.join("shutdown_failure.json"),
    }
}

pub(crate) fn daemon_log_hint() -> String {
    "run `holon daemon logs` for details".into()
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

pub fn config_fingerprint(config: &AppConfig) -> Result<String> {
    let payload = serde_json::json!({
        "default_agent_id": config.default_agent_id,
        "http_addr": config.http_addr,
        "callback_base_url": config.callback_base_url,
        "home_dir": config.home_dir,
        "socket_path": config.socket_path,
        "workspace_dir": config.workspace_dir,
        "context_window_messages": config.context_window_messages,
        "context_window_briefs": config.context_window_briefs,
        "compaction_trigger_messages": config.compaction_trigger_messages,
        "compaction_keep_recent_messages": config.compaction_keep_recent_messages,
        "prompt_budget_estimated_tokens": config.prompt_budget_estimated_tokens,
        "compaction_trigger_estimated_tokens": config.compaction_trigger_estimated_tokens,
        "compaction_keep_recent_estimated_tokens": config.compaction_keep_recent_estimated_tokens,
        "recent_episode_candidates": config.recent_episode_candidates,
        "max_relevant_episodes": config.max_relevant_episodes,
        "control_auth_mode": format!("{:?}", config.control_auth_mode),
        "control_token_configured": config.control_token.is_some(),
        "runtime_max_output_tokens": config.runtime_max_output_tokens,
        "default_tool_output_tokens": config.default_tool_output_tokens,
        "max_tool_output_tokens": config.max_tool_output_tokens,
        "disable_provider_fallback": config.provider_fallback_disabled(),
        "default_model": {
            "provider": config.default_model.provider.as_str(),
            "model": config.default_model.model,
        },
        "fallback_models": config.fallback_models.iter().map(|model| {
            serde_json::json!({
                "provider": model.provider.as_str(),
                "model": model.model,
            })
        }).collect::<Vec<_>>(),
        "providers": config.providers.iter().map(|(id, provider)| {
            serde_json::json!({
                "id": id.as_str(),
                "base_url": provider.base_url,
                "transport": provider.transport.as_str(),
                "auth": {
                    "source": provider.auth.source.as_str(),
                    "kind": provider.auth.kind.as_str(),
                    "env": provider.auth.env,
                    "profile": provider.auth.profile,
                    "external": provider.auth.external,
                    "credential_configured": provider.has_configured_credential(),
                },
            })
        }).collect::<Vec<_>>()
    });
    let encoded = serde_json::to_vec(&payload)?;
    let mut hasher = Sha256::new();
    hasher.update(encoded);
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn load_daemon_metadata(config: &AppConfig) -> Result<Option<RuntimeServiceMetadata>> {
    let path = daemon_paths(config).metadata_path;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let metadata = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to decode {}", path.display()))?;
    Ok(Some(metadata))
}

pub fn load_last_runtime_failure(config: &AppConfig) -> Result<Option<RuntimeFailureSummary>> {
    let path = daemon_paths(config).last_failure_path;
    if !path.exists() {
        return Ok(None);
    }
    load_runtime_failure_file(&path)
}

pub(crate) fn persist_last_runtime_failure(
    config: &AppConfig,
    failure: &RuntimeFailureSummary,
) -> Result<()> {
    let path = daemon_paths(config).last_failure_path;
    persist_runtime_failure_file(config, &path, failure)
}

fn load_startup_failure(config: &AppConfig) -> Result<Option<RuntimeFailureSummary>> {
    load_runtime_failure_file(&daemon_paths(config).startup_failure_path)
}

fn load_shutdown_failure(config: &AppConfig) -> Result<Option<RuntimeFailureSummary>> {
    load_runtime_failure_file(&daemon_paths(config).shutdown_failure_path)
}

fn load_runtime_failure_file(path: &Path) -> Result<Option<RuntimeFailureSummary>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let failure = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to decode {}", path.display()))?;
    Ok(Some(failure))
}

fn persist_runtime_failure_file(
    config: &AppConfig,
    path: &Path,
    failure: &RuntimeFailureSummary,
) -> Result<()> {
    fs::create_dir_all(config.run_dir())
        .with_context(|| format!("failed to create {}", config.run_dir().display()))?;
    fs::write(path, serde_json::to_vec_pretty(failure)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn persist_daemon_lifecycle_failure(
    config: &AppConfig,
    failure: &RuntimeFailureSummary,
) -> Result<()> {
    persist_last_runtime_failure(config, failure)?;
    let phase_path = match failure.phase {
        RuntimeFailurePhase::Startup => daemon_paths(config).startup_failure_path,
        RuntimeFailurePhase::Shutdown => daemon_paths(config).shutdown_failure_path,
        RuntimeFailurePhase::RuntimeTurn => return Ok(()),
    };
    persist_runtime_failure_file(config, &phase_path, failure)
}

pub(crate) fn clear_persisted_daemon_lifecycle_failures(config: &AppConfig) -> Result<()> {
    let paths = daemon_paths(config);
    remove_path_if_exists(&paths.last_failure_path)?;
    remove_path_if_exists(&paths.startup_failure_path)?;
    remove_path_if_exists(&paths.shutdown_failure_path)?;
    Ok(())
}

fn latest_public_runtime_failure(config: &AppConfig) -> Result<Option<RuntimeFailureSummary>> {
    let host_storage = AppStorage::new(config.home_dir.join("host"))?;
    let mut latest = None;
    for identity in host_storage
        .latest_agent_identities()?
        .into_iter()
        .filter(|record| {
            record.status == AgentRegistryStatus::Active
                && record.visibility == AgentVisibility::Public
        })
    {
        let primary = config.agent_root_dir().join(&identity.agent_id);
        let legacy = config.data_dir.join("sessions").join(&identity.agent_id);
        let storage = AppStorage::new(if primary.exists() || !legacy.exists() {
            primary
        } else {
            legacy
        })?;
        let failure = storage
            .read_agent()?
            .and_then(|agent| agent.last_runtime_failure);
        latest = merge_latest_failure(latest, failure);
    }
    Ok(latest)
}

pub(crate) fn latest_known_runtime_failure(
    config: &AppConfig,
) -> Result<Option<RuntimeFailureSummary>> {
    Ok(merge_latest_failure(
        latest_public_runtime_failure(config)?,
        load_last_runtime_failure(config)?,
    ))
}

pub fn cleanup_daemon_state(config: &AppConfig) -> Result<()> {
    let paths = daemon_paths(config);
    remove_path_if_exists(&paths.pid_path)?;
    remove_path_if_exists(&paths.metadata_path)?;
    #[cfg(unix)]
    remove_path_if_exists(&paths.socket_path)?;
    Ok(())
}

fn read_daemon_log_tail(config: &AppConfig, tail_lines: usize) -> Result<Vec<String>> {
    let log_path = daemon_paths(config).log_path;
    if tail_lines == 0 {
        return Ok(Vec::new());
    }
    let mut file = match fs::File::open(&log_path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", log_path.display()))
        }
    };
    let file_len = file
        .metadata()
        .with_context(|| format!("failed to stat {}", log_path.display()))?
        .len();
    let mut start_offset = file_len;
    let mut bytes_scanned = 0usize;
    let mut newline_count = 0usize;
    let mut bytes = Vec::new();
    while start_offset > 0
        && bytes_scanned < DAEMON_LOG_TAIL_READ_BYTE_LIMIT
        && newline_count <= tail_lines
    {
        let remaining_budget = DAEMON_LOG_TAIL_READ_BYTE_LIMIT - bytes_scanned;
        let chunk_size = remaining_budget
            .min(DAEMON_LOG_TAIL_READ_CHUNK_SIZE)
            .min(start_offset as usize);
        start_offset -= chunk_size as u64;
        file.seek(SeekFrom::Start(start_offset))
            .with_context(|| format!("failed to seek {}", log_path.display()))?;
        let mut chunk = vec![0; chunk_size];
        file.read_exact(&mut chunk)
            .with_context(|| format!("failed to read {}", log_path.display()))?;
        newline_count += chunk.iter().filter(|&&byte| byte == b'\n').count();
        chunk.extend(bytes);
        bytes = chunk;
        bytes_scanned += chunk_size;
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text
        .lines()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    if start_offset > 0 && !text.starts_with('\n') && !lines.is_empty() {
        lines[0] = truncate_tail_line(&format!("...{}", lines[0]));
    }
    for line in &mut lines {
        *line = truncate_tail_line(line);
    }
    if lines.len() > tail_lines {
        return Ok(lines.split_off(lines.len() - tail_lines));
    }
    Ok(lines)
}

fn truncate_tail_line(line: &str) -> String {
    let char_count = line.chars().count();
    if char_count <= DAEMON_LOG_TAIL_LINE_CHAR_LIMIT {
        return line.to_string();
    }
    let keep = DAEMON_LOG_TAIL_LINE_CHAR_LIMIT.saturating_sub(3);
    let start = char_count.saturating_sub(keep);
    let suffix = line
        .char_indices()
        .nth(start)
        .map(|(index, _)| &line[index..])
        .unwrap_or(line);
    format!("...{suffix}")
}

pub fn daemon_logs(config: &AppConfig, tail_lines: usize) -> Result<DaemonLogsView> {
    let paths = daemon_paths(config);
    let runtime_metadata = load_daemon_metadata(config).ok().flatten();
    let last_failure = latest_known_runtime_failure(config).ok().flatten();
    let startup_failure = load_startup_failure(config).ok().flatten();
    let shutdown_failure = load_shutdown_failure(config).ok().flatten();
    let tail = read_daemon_log_tail(config, tail_lines)?;
    let message = if tail_lines == 0 {
        "daemon log tail omitted (--tail 0)".into()
    } else if tail.is_empty() {
        "no daemon log lines are currently available".into()
    } else {
        format!("showing the last {} daemon log lines", tail.len())
    };
    Ok(DaemonLogsView {
        ok: true,
        log_path: paths.log_path,
        metadata_path: paths.metadata_path,
        last_failure_path: paths.last_failure_path,
        startup_failure_path: paths.startup_failure_path,
        shutdown_failure_path: paths.shutdown_failure_path,
        runtime_metadata,
        last_failure,
        startup_failure,
        shutdown_failure,
        tail,
        message,
    })
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to stat {}", path.display()));
        }
    };
    if metadata.is_dir() {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to remove {}", path.display()))
        }
    }
    Ok(())
}

pub(crate) fn stale_files(config: &AppConfig) -> Vec<PathBuf> {
    let paths = daemon_paths(config);
    let mut stale = Vec::new();
    if paths.pid_path.exists() {
        stale.push(paths.pid_path);
    }
    if paths.metadata_path.exists() {
        stale.push(paths.metadata_path);
    }
    #[cfg(unix)]
    if paths.socket_path.exists() {
        stale.push(paths.socket_path);
    }
    stale
}

pub(crate) fn read_daemon_log_excerpt(config: &AppConfig) -> String {
    read_daemon_log_tail(config, 20)
        .ok()
        .and_then(|lines| {
            lines
                .into_iter()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(|line| line.trim().to_string())
        })
        .unwrap_or_else(daemon_log_hint)
}
