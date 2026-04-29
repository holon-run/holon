use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::{anyhow, Result};

use crate::{
    config::AppConfig,
    storage::AppStorage,
    system::WorkspaceAccessMode,
    types::{AgentIdentityRecord, WorkspaceEntry, WorkspaceOccupancyRecord},
};

#[derive(Clone)]
pub(crate) struct RuntimeRegistry {
    inner: Arc<RuntimeRegistryInner>,
}

struct RuntimeRegistryInner {
    config: AppConfig,
    host_storage: AppStorage,
    agent_identities: StdRwLock<HashMap<String, AgentIdentityRecord>>,
}

impl RuntimeRegistry {
    pub(crate) fn new(config: AppConfig) -> Result<Self> {
        let host_storage = AppStorage::new(config.home_dir.join("host"))?;
        let agent_identities = host_storage
            .latest_agent_identities()?
            .into_iter()
            .map(|record| (record.agent_id.clone(), record))
            .collect();
        Ok(Self {
            inner: Arc::new(RuntimeRegistryInner {
                config,
                host_storage,
                agent_identities: StdRwLock::new(agent_identities),
            }),
        })
    }

    pub(crate) fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    pub(crate) fn agent_identity_record(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentIdentityRecord>> {
        Ok(self
            .inner
            .agent_identities
            .read()
            .expect("agent identities cache poisoned")
            .get(agent_id)
            .cloned())
    }

    pub(crate) fn agent_identity_records(&self) -> Result<Vec<AgentIdentityRecord>> {
        let mut records = self
            .inner
            .agent_identities
            .read()
            .expect("agent identities cache poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(records)
    }

    pub(crate) fn append_agent_identity(&self, record: &AgentIdentityRecord) -> Result<()> {
        self.inner.host_storage.append_agent_identity(record)?;
        self.inner
            .agent_identities
            .write()
            .expect("agent identities cache poisoned")
            .insert(record.agent_id.clone(), record.clone());
        Ok(())
    }

    pub(crate) fn workspace_entries(&self) -> Result<Vec<WorkspaceEntry>> {
        self.inner.host_storage.latest_workspace_entries()
    }

    pub(crate) fn workspace_occupancies(&self) -> Result<Vec<WorkspaceOccupancyRecord>> {
        self.inner.host_storage.latest_workspace_occupancies()
    }

    pub(crate) fn append_workspace_occupancy(
        &self,
        record: &WorkspaceOccupancyRecord,
    ) -> Result<()> {
        self.inner.host_storage.append_workspace_occupancy(record)
    }

    pub(crate) fn workspace_occupancy_by_id(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        Ok(self
            .workspace_occupancies()?
            .into_iter()
            .find(|record| record.occupancy_id == occupancy_id))
    }

    pub(crate) fn active_workspace_occupancies_for_root(
        &self,
        execution_root_id: &str,
    ) -> Result<Vec<WorkspaceOccupancyRecord>> {
        Ok(self
            .workspace_occupancies()?
            .into_iter()
            .filter(|record| {
                record.execution_root_id == execution_root_id && record.released_at.is_none()
            })
            .collect())
    }

    pub(crate) fn acquire_workspace_occupancy(
        &self,
        workspace_id: &str,
        execution_root_id: &str,
        holder_agent_id: &str,
        access_mode: WorkspaceAccessMode,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        let active = self.active_workspace_occupancies_for_root(execution_root_id)?;
        if let Some(existing) = active.iter().find(|record| {
            record.holder_agent_id == holder_agent_id && record.access_mode == access_mode
        }) {
            return Ok(Some(existing.clone()));
        }
        if access_mode == WorkspaceAccessMode::ExclusiveWrite
            && active.iter().any(|record| {
                record.holder_agent_id != holder_agent_id
                    && record.access_mode == WorkspaceAccessMode::ExclusiveWrite
            })
        {
            return Err(anyhow!(
                "execution root {} already has an exclusive_write holder",
                execution_root_id
            ));
        }
        let record = WorkspaceOccupancyRecord {
            occupancy_id: format!("occ-{}", uuid::Uuid::new_v4().simple()),
            execution_root_id: execution_root_id.to_string(),
            workspace_id: workspace_id.to_string(),
            holder_agent_id: holder_agent_id.to_string(),
            access_mode,
            acquired_at: chrono::Utc::now(),
            released_at: None,
        };
        self.append_workspace_occupancy(&record)?;
        Ok(Some(record))
    }

    pub(crate) fn release_workspace_occupancy(
        &self,
        occupancy_id: &str,
    ) -> Result<Option<WorkspaceOccupancyRecord>> {
        let Some(mut record) = self.workspace_occupancy_by_id(occupancy_id)? else {
            return Ok(None);
        };
        if record.released_at.is_some() {
            return Ok(Some(record));
        }
        record.released_at = Some(chrono::Utc::now());
        self.append_workspace_occupancy(&record)?;
        Ok(Some(record))
    }

    pub(crate) fn ensure_workspace_entry(
        &self,
        workspace_anchor: PathBuf,
    ) -> Result<WorkspaceEntry> {
        let workspace_anchor = crate::system::workspace::normalize_path(&workspace_anchor)?;
        if let Some(existing) = self
            .inner
            .host_storage
            .latest_workspace_entries()?
            .into_iter()
            .find(|entry| entry.workspace_anchor == workspace_anchor)
        {
            return Ok(existing);
        }

        let repo_name = workspace_anchor
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string);
        let entry = WorkspaceEntry::new(
            format!("ws-{}", uuid::Uuid::new_v4().simple()),
            workspace_anchor,
            repo_name,
        );
        self.inner.host_storage.append_workspace_entry(&entry)?;
        Ok(entry)
    }

    pub(crate) fn ensure_default_agent_identity(&self) -> Result<AgentIdentityRecord> {
        self.validate_agent_id(&self.inner.config.default_agent_id)?;
        if let Some(existing) = self.agent_identity_record(&self.inner.config.default_agent_id)? {
            return Ok(existing);
        }
        let record = AgentIdentityRecord::new(
            self.inner.config.default_agent_id.clone(),
            crate::types::AgentKind::Default,
            crate::types::AgentVisibility::Public,
            crate::types::AgentOwnership::SelfOwned,
            crate::types::AgentProfilePreset::PublicNamed,
            None,
            None,
        );
        self.append_agent_identity(&record)?;
        Ok(record)
    }

    pub(crate) fn validate_agent_id(&self, agent_id: &str) -> Result<()> {
        validate_agent_id_format(agent_id)
    }
}

pub(crate) fn validate_agent_id_format(agent_id: &str) -> Result<()> {
    use std::path::{Component, Path};

    if agent_id.is_empty() {
        return Err(anyhow!("agent id must not be empty"));
    }
    if !agent_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(anyhow!(
            "agent id must contain only ASCII letters, digits, '.', '-', or '_'"
        ));
    }

    let mut components = Path::new(agent_id).components();
    let valid_component =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !valid_component {
        return Err(anyhow!("agent id must be a single normal path component"));
    }
    Ok(())
}
