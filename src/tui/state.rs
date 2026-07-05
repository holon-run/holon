use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{client::LocalClient, operator_event::OperatorDisplayMode};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct TuiClientState {
    pub(super) last_selected_agent_id: String,
    #[serde(default)]
    pub(super) display: TuiDisplayState,
    pub(super) updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct TuiDisplayState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) per_agent: BTreeMap<String, OperatorDisplayMode>,
}

impl TuiClientState {
    pub(super) fn new(last_selected_agent_id: impl Into<String>) -> Self {
        Self {
            last_selected_agent_id: last_selected_agent_id.into(),
            display: TuiDisplayState::default(),
            updated_at: Utc::now(),
        }
    }

    pub(super) fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read TUI state {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to decode TUI state {}", path.display()))
    }

    pub(super) fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create TUI state dir {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes)
            .with_context(|| format!("failed to write TUI state {}", path.display()))
    }

    pub(super) fn load_or_new(path: &Path, last_selected_agent_id: impl Into<String>) -> Self {
        Self::load(path).unwrap_or_else(|_| Self::new(last_selected_agent_id))
    }

    pub(super) fn effective_display_mode(&self, agent_id: &str) -> OperatorDisplayMode {
        self.display
            .per_agent
            .get(agent_id)
            .copied()
            .unwrap_or(OperatorDisplayMode::DEFAULT)
    }

    pub(super) fn set_selected_agent(&mut self, agent_id: impl Into<String>) {
        self.last_selected_agent_id = agent_id.into();
        self.updated_at = Utc::now();
    }

    pub(super) fn set_agent_display_mode(
        &mut self,
        agent_id: impl Into<String>,
        display_mode: OperatorDisplayMode,
    ) {
        self.display.per_agent.insert(agent_id.into(), display_mode);
        self.updated_at = Utc::now();
    }

    pub(super) fn clear_agent_display_mode(&mut self, agent_id: &str) {
        self.display.per_agent.remove(agent_id);
        self.updated_at = Utc::now();
    }
}

pub(super) fn tui_state_path(client: &LocalClient) -> PathBuf {
    let filename = match client.remote_base_url() {
        Some(base_url) => format!("remote-{}.json", short_sha256_hex(base_url)),
        None => "local.json".to_string(),
    };
    client.home_dir().join("state").join("tui").join(filename)
}

fn short_sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip_preserves_selected_agent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state").join("tui").join("local.json");
        let mut state = TuiClientState::new("agent-beta");
        state.set_agent_display_mode("agent-beta", OperatorDisplayMode::Verbose);

        state.save(&path).unwrap();
        let loaded = TuiClientState::load(&path).unwrap();

        assert_eq!(loaded.last_selected_agent_id, "agent-beta");
        assert_eq!(
            loaded.effective_display_mode("agent-beta"),
            OperatorDisplayMode::Verbose
        );
    }

    #[test]
    fn state_loads_legacy_selected_agent_only_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("local.json");
        fs::write(
            &path,
            r#"{
  "last_selected_agent_id": "agent-beta",
  "updated_at": "2026-01-02T03:04:05Z"
}"#,
        )
        .unwrap();

        let loaded = TuiClientState::load(&path).unwrap();

        assert_eq!(loaded.last_selected_agent_id, "agent-beta");
        assert_eq!(
            loaded.effective_display_mode("agent-beta"),
            OperatorDisplayMode::DEFAULT
        );
    }
}
