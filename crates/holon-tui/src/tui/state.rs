use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::client::LocalClient;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct TuiClientState {
    pub(super) last_selected_agent_id: String,
    pub(super) updated_at: DateTime<Utc>,
}

impl TuiClientState {
    pub(super) fn new(last_selected_agent_id: impl Into<String>) -> Self {
        Self {
            last_selected_agent_id: last_selected_agent_id.into(),
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

    pub(super) fn set_selected_agent(&mut self, agent_id: impl Into<String>) {
        self.last_selected_agent_id = agent_id.into();
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
mod tests;
