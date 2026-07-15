use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock as StdRwLock};

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;

use crate::{
    config::AppConfig,
    ids,
    runtime_db::RuntimeDb,
    storage::AppStorage,
    system::WorkspaceAccessMode,
    types::{
        agent_home_workspace_id, AgentIdentityRecord, WorkspaceEntry, WorkspaceOccupancyRecord,
    },
};

#[derive(Clone)]
pub(crate) struct RuntimeRegistry {
    inner: Arc<RuntimeRegistryInner>,
}

struct RuntimeRegistryInner {
    config: ArcSwap<AppConfig>,
    host_storage: AppStorage,
    agent_identities: StdRwLock<HashMap<String, AgentIdentityRecord>>,
}

impl RuntimeRegistry {
    pub(crate) fn new(config: AppConfig, runtime_db: RuntimeDb) -> Result<Self> {
        let host_storage =
            AppStorage::new_global(config.home_dir.join("host"), runtime_db.clone())?;
        if !runtime_db.storage_domain_is_complete("workspace_entries", "db")? {
            runtime_db
                .workspace_entries()
                .import_legacy(host_storage.read_recent_workspace_entries(usize::MAX)?)?;
        }
        if !runtime_db.storage_domain_is_complete("workspace_occupancies", "db")? {
            runtime_db
                .workspace_occupancies()
                .import_legacy(host_storage.read_recent_workspace_occupancies(usize::MAX)?)?;
        }
        if !runtime_db.storage_domain_is_complete("agent_identities", "db")? {
            runtime_db
                .agent_identities()
                .import_legacy(host_storage.read_recent_agent_identities(usize::MAX)?)?;
        }
        let agent_identities = host_storage
            .latest_agent_identities()?
            .into_iter()
            .map(|record| (record.agent_id.clone(), record))
            .collect();
        Ok(Self {
            inner: Arc::new(RuntimeRegistryInner {
                config: ArcSwap::from_pointee(config),
                host_storage,
                agent_identities: StdRwLock::new(agent_identities),
            }),
        })
    }

    pub(crate) fn config(&self) -> Arc<AppConfig> {
        self.inner.config.load_full()
    }

    pub(crate) fn replace_config(&self, config: AppConfig) {
        self.inner.config.store(Arc::new(config));
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
            occupancy_id: ids::workspace_occupancy_id(),
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
        let agent_home = self.agent_home_workspace_entry_for_anchor(&workspace_anchor)?;
        let det_id = agent_home
            .as_ref()
            .map(|entry| entry.workspace_id.clone())
            .unwrap_or_else(|| ids::deterministic_workspace_id(&workspace_anchor));
        if let Some(existing) = self
            .inner
            .host_storage
            .latest_workspace_entries()?
            .into_iter()
            .find(|entry| entry.workspace_anchor == workspace_anchor)
        {
            if existing.workspace_id != det_id {
                return Err(anyhow!(
                    "unsupported legacy workspace ID '{}' for '{}'; expected deterministic ID '{}'. \
                     This database predates the supported workspace ID migration window",
                    existing.workspace_id,
                    workspace_anchor.display(),
                    det_id
                ));
            }
            if let Some(entry) = agent_home {
                if existing.workspace_alias != entry.workspace_alias
                    || existing.workspace_kind != entry.workspace_kind
                    || existing.owner_agent_id != entry.owner_agent_id
                {
                    self.inner.host_storage.append_workspace_entry(&entry)?;
                    return Ok(entry);
                }
            }
            return Ok(existing);
        }

        if let Some(entry) = agent_home {
            self.inner.host_storage.append_workspace_entry(&entry)?;
            return Ok(entry);
        }
        let repo_name = workspace_anchor
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToString::to_string);
        let entry = WorkspaceEntry::new(det_id, workspace_anchor, repo_name);
        self.inner.host_storage.append_workspace_entry(&entry)?;
        Ok(entry)
    }

    fn agent_home_workspace_entry_for_anchor(
        &self,
        workspace_anchor: &std::path::Path,
    ) -> Result<Option<WorkspaceEntry>> {
        let agents_root =
            crate::system::workspace::normalize_path(&self.config().data_dir.join("agents"))?;
        let Ok(agent_id_path) = workspace_anchor.strip_prefix(&agents_root) else {
            return Ok(None);
        };
        if agent_id_path.components().count() != 1 {
            return Ok(None);
        }
        let Some(agent_id) = agent_id_path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if self.agent_identity_record(agent_id)?.is_none()
            && agent_id != self.config().default_agent_id
        {
            return Ok(None);
        }
        let mut entry = WorkspaceEntry::new(
            agent_home_workspace_id(agent_id),
            workspace_anchor.to_path_buf(),
            Some("AgentHome".into()),
        );
        entry.workspace_alias = Some(crate::types::AGENT_HOME_WORKSPACE_ID.into());
        entry.workspace_kind = Some("agent_home".into());
        entry.owner_agent_id = Some(agent_id.to_string());
        Ok(Some(entry))
    }

    pub(crate) fn ensure_default_agent_identity(&self) -> Result<AgentIdentityRecord> {
        let config = self.config();
        self.validate_agent_id(&config.default_agent_id)?;
        if let Some(existing) = self.agent_identity_record(&config.default_agent_id)? {
            return Ok(existing);
        }
        let record = AgentIdentityRecord::new(
            config.default_agent_id.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::runtime_db::RuntimeDb;
    use tempfile::tempdir;

    fn test_registry() -> (tempfile::TempDir, RuntimeRegistry) {
        let home = tempdir().unwrap();
        std::fs::write(
            home.path().join("config.json"),
            r#"{"model":{"default":"openai/gpt-5.4"}}"#,
        )
        .unwrap();
        let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
        let runtime_db =
            RuntimeDb::open_and_migrate(config.runtime_db_path(), config.runtime_db_lock_path())
                .unwrap();
        let registry = RuntimeRegistry::new(config, runtime_db).unwrap();
        (home, registry)
    }

    const ROOT_ID: &str = "canonical_root:ws_test";
    const WS_ID: &str = "ws_test";

    // ── SharedRead conflict matrix ──

    #[test]
    fn multiple_shared_read_on_same_root_succeed() {
        let (_home, registry) = test_registry();

        let r1 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-a", WorkspaceAccessMode::SharedRead)
            .unwrap();
        assert!(r1.is_some());

        let r2 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-b", WorkspaceAccessMode::SharedRead)
            .unwrap();
        assert!(r2.is_some());

        // Both should be active.
        let active = registry
            .active_workspace_occupancies_for_root(ROOT_ID)
            .unwrap();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn same_agent_same_mode_acquiry_is_idempotent() {
        let (_home, registry) = test_registry();

        let r1 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-a", WorkspaceAccessMode::SharedRead)
            .unwrap()
            .unwrap();

        let r2 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-a", WorkspaceAccessMode::SharedRead)
            .unwrap()
            .unwrap();

        // Should return the same record, not create a new one.
        assert_eq!(r1.occupancy_id, r2.occupancy_id);

        let active = registry
            .active_workspace_occupancies_for_root(ROOT_ID)
            .unwrap();
        assert_eq!(
            active.len(),
            1,
            "idempotent acquire should not create duplicate"
        );
    }

    // ── ExclusiveWrite conflict matrix ──

    #[test]
    fn exclusive_write_blocks_other_exclusive_write() {
        let (_home, registry) = test_registry();

        registry
            .acquire_workspace_occupancy(
                WS_ID,
                ROOT_ID,
                "agent-a",
                WorkspaceAccessMode::ExclusiveWrite,
            )
            .unwrap();

        let err = registry
            .acquire_workspace_occupancy(
                WS_ID,
                ROOT_ID,
                "agent-b",
                WorkspaceAccessMode::ExclusiveWrite,
            )
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("already has an exclusive_write holder"),
            "second exclusive_write from different agent should fail"
        );
    }

    #[test]
    fn same_agent_exclusive_write_is_idempotent() {
        let (_home, registry) = test_registry();

        let r1 = registry
            .acquire_workspace_occupancy(
                WS_ID,
                ROOT_ID,
                "agent-a",
                WorkspaceAccessMode::ExclusiveWrite,
            )
            .unwrap()
            .unwrap();

        // Same agent + same mode should be idempotent even for ExclusiveWrite.
        let r2 = registry
            .acquire_workspace_occupancy(
                WS_ID,
                ROOT_ID,
                "agent-a",
                WorkspaceAccessMode::ExclusiveWrite,
            )
            .unwrap()
            .unwrap();

        assert_eq!(r1.occupancy_id, r2.occupancy_id);
    }

    #[test]
    fn exclusive_write_then_shared_read_from_other_agent_allowed() {
        // Documents current behavior: ExclusiveWrite does not block SharedRead
        // from another agent. The conflict check only blocks ExclusiveWrite × ExclusiveWrite.
        let (_home, registry) = test_registry();

        registry
            .acquire_workspace_occupancy(
                WS_ID,
                ROOT_ID,
                "agent-a",
                WorkspaceAccessMode::ExclusiveWrite,
            )
            .unwrap();

        let r2 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-b", WorkspaceAccessMode::SharedRead)
            .unwrap();
        assert!(
            r2.is_some(),
            "SharedRead from another agent is allowed alongside ExclusiveWrite"
        );
    }

    // ── Release behavior ──

    #[test]
    fn release_marks_occupancy_as_released() {
        let (_home, registry) = test_registry();

        let record = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-a", WorkspaceAccessMode::SharedRead)
            .unwrap()
            .unwrap();

        let released = registry
            .release_workspace_occupancy(&record.occupancy_id)
            .unwrap();
        assert!(released.is_some());
        assert!(released.unwrap().released_at.is_some());

        let active = registry
            .active_workspace_occupancies_for_root(ROOT_ID)
            .unwrap();
        assert!(
            active.is_empty(),
            "released occupancy should not appear in active list"
        );
    }

    #[test]
    fn release_is_idempotent() {
        let (_home, registry) = test_registry();

        let record = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-a", WorkspaceAccessMode::SharedRead)
            .unwrap()
            .unwrap();

        // First release.
        let first = registry
            .release_workspace_occupancy(&record.occupancy_id)
            .unwrap();
        assert!(first.is_some());

        // Second release should also return the record (already released).
        let second = registry
            .release_workspace_occupancy(&record.occupancy_id)
            .unwrap();
        assert!(second.is_some());
        assert!(second.unwrap().released_at.is_some());
    }

    #[test]
    fn release_nonexistent_id_returns_none() {
        let (_home, registry) = test_registry();
        let result = registry
            .release_workspace_occupancy("occ_nonexistent")
            .unwrap();
        assert!(result.is_none());
    }

    // ── active_workspace_occupancies_for_root ──

    #[test]
    fn active_occupancies_filter_excludes_released() {
        let (_home, registry) = test_registry();

        let r1 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-a", WorkspaceAccessMode::SharedRead)
            .unwrap()
            .unwrap();
        let _r2 = registry
            .acquire_workspace_occupancy(WS_ID, ROOT_ID, "agent-b", WorkspaceAccessMode::SharedRead)
            .unwrap()
            .unwrap();

        // Release one.
        registry
            .release_workspace_occupancy(&r1.occupancy_id)
            .unwrap();

        let active = registry
            .active_workspace_occupancies_for_root(ROOT_ID)
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].holder_agent_id, "agent-b");
    }

    #[test]
    fn active_occupancies_filter_by_root_id() {
        let (_home, registry) = test_registry();

        registry
            .acquire_workspace_occupancy(
                WS_ID,
                "canonical_root:ws_a",
                "agent-a",
                WorkspaceAccessMode::SharedRead,
            )
            .unwrap();
        registry
            .acquire_workspace_occupancy(
                WS_ID,
                "canonical_root:ws_b",
                "agent-b",
                WorkspaceAccessMode::SharedRead,
            )
            .unwrap();

        let active_a = registry
            .active_workspace_occupancies_for_root("canonical_root:ws_a")
            .unwrap();
        assert_eq!(active_a.len(), 1);
        assert_eq!(active_a[0].holder_agent_id, "agent-a");
    }

    // ── ensure_workspace_entry ──

    #[test]
    fn ensure_workspace_entry_creates_deterministic_id() {
        let (_home, registry) = test_registry();
        let dir = tempdir().unwrap();

        let entry = registry
            .ensure_workspace_entry(dir.path().to_path_buf())
            .unwrap();
        let expected_id = ids::deterministic_workspace_id(dir.path());
        assert_eq!(entry.workspace_id, expected_id);
    }

    #[test]
    fn ensure_workspace_entry_is_idempotent() {
        let (_home, registry) = test_registry();
        let dir = tempdir().unwrap();

        let entry1 = registry
            .ensure_workspace_entry(dir.path().to_path_buf())
            .unwrap();
        let entry2 = registry
            .ensure_workspace_entry(dir.path().to_path_buf())
            .unwrap();

        assert_eq!(entry1.workspace_id, entry2.workspace_id);
    }

    #[test]
    fn ensure_workspace_entry_rejects_legacy_random_id_without_migrating() {
        let (_home, registry) = test_registry();
        let dir = tempdir().unwrap();
        let legacy = WorkspaceEntry::new(
            "legacy-random-workspace-id",
            dir.path().to_path_buf(),
            Some("legacy-workspace".into()),
        );
        registry
            .inner
            .host_storage
            .append_workspace_entry(&legacy)
            .unwrap();

        let error = registry
            .ensure_workspace_entry(dir.path().to_path_buf())
            .unwrap_err();
        let expected_id = ids::deterministic_workspace_id(dir.path());
        assert!(
            error
                .to_string()
                .contains("unsupported legacy workspace ID"),
            "unexpected error: {error:#}"
        );
        assert!(
            error.to_string().contains(&expected_id),
            "error should identify the expected deterministic ID: {error:#}"
        );

        let entries = registry.workspace_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].workspace_id, legacy.workspace_id);
    }

    #[test]
    fn ensure_workspace_entry_extracts_repo_name() {
        let (_home, registry) = test_registry();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("my-repo-name")).unwrap();
        let repo_path = dir.path().join("my-repo-name");

        let entry = registry.ensure_workspace_entry(repo_path).unwrap();
        assert_eq!(entry.repo_name.as_deref(), Some("my-repo-name"));
    }

    #[test]
    fn ensure_workspace_entry_recognizes_agent_home_path() {
        let (_home, registry) = test_registry();
        registry.ensure_default_agent_identity().unwrap();
        let agent_id = registry.config().default_agent_id.clone();
        let agent_home = registry.config().agent_root_dir().join(&agent_id);
        std::fs::create_dir_all(&agent_home).unwrap();

        let entry = registry.ensure_workspace_entry(agent_home.clone()).unwrap();

        assert_eq!(entry.workspace_id, agent_home_workspace_id(&agent_id));
        assert_eq!(entry.workspace_alias.as_deref(), Some("agent_home"));
        assert_eq!(entry.workspace_kind.as_deref(), Some("agent_home"));
        assert_eq!(entry.owner_agent_id.as_deref(), Some(agent_id.as_str()));
        assert_eq!(
            entry.workspace_id,
            registry
                .ensure_workspace_entry(agent_home)
                .unwrap()
                .workspace_id
        );
    }
}
