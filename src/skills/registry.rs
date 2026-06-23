//! Skills registry: skill root registration and catalog management.
//!
//! This module implements a registry for skill discovery across multiple roots,
//! supporting user-global, agent-local, and workspace-local skill sources.
//! It maintains registration metadata and provides catalog views with scope filtering.

use anyhow::Result;
use chrono::Utc;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tracing::warn;

use crate::types::{
    SkillCatalogEntry, SkillRootRegistration, SkillRootScanStatus, SkillRootSourceKind,
    SkillRootWatchStatus, SkillScope,
};

/// Read-only skills registry.
///
/// The registry maintains skill entries from registered roots and provides
/// a catalog view with precedence resolution (agent > workspace > user).
#[derive(Debug, Clone, Default)]
pub struct SkillsRegistry {
    roots: Vec<SkillRootRegistration>,
    entries: Vec<SkillCatalogEntry>,
}

impl SkillsRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a skill root and scan it for skills.
    pub fn register_root(&mut self, registration: SkillRootRegistration) -> Result<()> {
        if !registration.root_path.exists() {
            return Ok(());
        }

        self.upsert_root(registration.clone());
        self.refresh_root(&registration.root_path)?;
        Ok(())
    }

    /// Replace the registered roots with the provided effective set.
    ///
    /// This keeps shared registries scoped to the current caller's effective
    /// roots instead of accumulating stale roots from other agents or workspaces.
    pub fn replace_roots(&mut self, registrations: Vec<SkillRootRegistration>) -> Result<()> {
        self.roots.clear();
        self.entries.clear();
        for registration in registrations {
            if registration.root_path.exists() {
                self.upsert_root(registration);
            }
        }
        self.rescan();
        Ok(())
    }

    /// Refresh a single registered root, replacing that root's snapshot entries.
    ///
    /// Scan failures are recorded on the root and remove stale entries for that
    /// root, but do not make the registry operation fail for catalog readers.
    pub fn refresh_root(&mut self, root_path: &Path) -> Result<bool> {
        let Some(root_index) = self.root_index(root_path) else {
            return Ok(false);
        };
        let registration = self.roots[root_index].clone();
        let (entries, scan_status) = Self::scan_root(&registration);

        self.entries
            .retain(|entry| !entry.path.starts_with(&registration.root_path));
        self.entries.extend(entries);
        self.roots[root_index].scan_status = scan_status;
        Ok(true)
    }

    /// Rescan all registered roots and replace the complete registry snapshot.
    ///
    /// Per-root scan failures are recorded on the root status and do not make
    /// catalog reads fail.
    pub fn rescan(&mut self) {
        let mut next_entries = Vec::new();
        for root in &mut self.roots {
            let (entries, scan_status) = Self::scan_root(root);
            next_entries.extend(entries);
            root.scan_status = scan_status;
        }
        self.entries = next_entries;
    }

    /// Record watcher setup or delivery failure without affecting catalog reads.
    pub fn record_watcher_failure(
        &mut self,
        root_path: impl AsRef<Path>,
        error: impl Into<String>,
    ) -> bool {
        let Some(root_index) = self.root_index(root_path.as_ref()) else {
            return false;
        };
        let error = error.into();
        warn!(
            root = %self.roots[root_index].root_path.display(),
            error = %error,
            "skill root watcher failed; registry will rely on manual rescan"
        );
        self.roots[root_index].watch_status = SkillRootWatchStatus::Failed { error };
        true
    }

    /// Mark a registered root as actively watched.
    pub fn record_watcher_started(&mut self, root_path: impl AsRef<Path>) -> bool {
        let Some(root_index) = self.root_index(root_path.as_ref()) else {
            return false;
        };
        self.roots[root_index].watch_status = SkillRootWatchStatus::Watching;
        true
    }

    fn upsert_root(&mut self, registration: SkillRootRegistration) {
        if let Some(root_index) = self.root_index(&registration.root_path) {
            self.roots[root_index] = registration;
        } else {
            self.roots.push(registration);
        }
    }

    fn root_index(&self, root_path: &Path) -> Option<usize> {
        let root_path = normalize_path(root_path);
        self.roots
            .iter()
            .position(|root| normalize_path(&root.root_path) == root_path)
    }

    fn scan_root(
        registration: &SkillRootRegistration,
    ) -> (Vec<SkillCatalogEntry>, SkillRootScanStatus) {
        let scope = match registration.source_kind {
            SkillRootSourceKind::UserGlobal => SkillScope::User,
            SkillRootSourceKind::AgentHome => SkillScope::Agent,
            SkillRootSourceKind::Workspace => SkillScope::Workspace,
        };

        match crate::skills::load_catalog_for_scope(scope, &registration.root_path) {
            Ok(entries) => (
                entries,
                SkillRootScanStatus::Scanned {
                    at: Utc::now().timestamp(),
                },
            ),
            Err(error) => (
                Vec::new(),
                SkillRootScanStatus::Failed {
                    error: error.to_string(),
                },
            ),
        }
    }

    /// Get the current catalog with precedence applied.
    pub fn catalog(&self) -> Vec<SkillCatalogEntry> {
        self.catalog_with_filter(None)
    }

    /// Get catalog filtered by optional scope.
    pub fn catalog_with_filter(&self, scope: Option<SkillScope>) -> Vec<SkillCatalogEntry> {
        let filtered: Vec<&SkillCatalogEntry> = if let Some(filter_scope) = scope {
            self.entries
                .iter()
                .filter(|e| e.scope == filter_scope)
                .collect()
        } else {
            self.entries.iter().collect()
        };

        let selected_by_name: BTreeMap<String, &SkillCatalogEntry> =
            filtered
                .into_iter()
                .fold(BTreeMap::new(), |mut acc, entry| {
                    let existing = acc.get(&entry.name);
                    if existing.map_or(true, |existing| {
                        self.skill_wins_catalog_selection(entry, existing)
                    }) {
                        acc.insert(entry.name.clone(), entry);
                    }
                    acc
                });
        selected_by_name.into_values().cloned().collect()
    }

    /// Get all registered roots.
    pub fn roots(&self) -> &[SkillRootRegistration] {
        &self.roots
    }

    fn skill_wins_catalog_selection(
        &self,
        candidate: &SkillCatalogEntry,
        existing: &SkillCatalogEntry,
    ) -> bool {
        let candidate_precedence = self.skill_precedence(candidate.scope);
        let existing_precedence = self.skill_precedence(existing.scope);
        candidate_precedence > existing_precedence
            || (candidate_precedence == existing_precedence
                && (&candidate.skill_id, &candidate.path) < (&existing.skill_id, &existing.path))
    }

    fn skill_precedence(&self, scope: SkillScope) -> u8 {
        match scope {
            SkillScope::Agent => 3,
            SkillScope::Workspace => 2,
            SkillScope::User => 1,
        }
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    path.components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use tempfile::TempDir;

    fn registration(root_path: PathBuf, source_kind: SkillRootSourceKind) -> SkillRootRegistration {
        SkillRootRegistration {
            source_kind,
            owner_agent_id: None,
            root_path,
            scan_status: SkillRootScanStatus::NeverScanned,
            watch_status: SkillRootWatchStatus::NotWatched,
        }
    }

    fn write_skill(root: &Path, dirname: &str, name: &str, description: &str) {
        let skill_dir = root.join(dirname);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody\n"),
        )
        .unwrap();
    }

    #[test]
    fn test_registry_new() {
        let registry = SkillsRegistry::new();
        assert!(registry.catalog().is_empty());
        assert!(registry.roots().is_empty());
    }

    #[test]
    fn test_register_nonexistent_root() {
        let mut registry = SkillsRegistry::new();
        let result = registry.register_root(registration(
            PathBuf::from("/nonexistent"),
            SkillRootSourceKind::UserGlobal,
        ));
        assert!(result.is_ok());
        assert!(registry.catalog().is_empty());
        assert!(registry.roots().is_empty());
    }

    #[test]
    fn test_catalog_precedence() {
        let mut registry = SkillsRegistry::new();

        registry.entries.push(SkillCatalogEntry {
            skill_id: "user_skill".to_string(),
            name: "test".to_string(),
            description: "user".to_string(),
            path: PathBuf::from("/user/test"),
            scope: SkillScope::User,
        });

        registry.entries.push(SkillCatalogEntry {
            skill_id: "agent_skill".to_string(),
            name: "test".to_string(),
            description: "agent".to_string(),
            path: PathBuf::from("/agent/test"),
            scope: SkillScope::Agent,
        });

        let catalog = registry.catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].scope, SkillScope::Agent);
        assert_eq!(catalog[0].description, "agent");
    }

    #[test]
    fn test_catalog_with_filter() {
        let mut registry = SkillsRegistry::new();

        registry.entries.push(SkillCatalogEntry {
            skill_id: "skill1".to_string(),
            name: "skill_a".to_string(),
            description: "agent".to_string(),
            path: PathBuf::from("/a"),
            scope: SkillScope::Agent,
        });

        registry.entries.push(SkillCatalogEntry {
            skill_id: "skill2".to_string(),
            name: "skill_b".to_string(),
            description: "user".to_string(),
            path: PathBuf::from("/b"),
            scope: SkillScope::User,
        });

        let agent_catalog = registry.catalog_with_filter(Some(SkillScope::Agent));
        assert_eq!(agent_catalog.len(), 1);
        assert_eq!(agent_catalog[0].scope, SkillScope::Agent);
    }

    #[test]
    fn refresh_root_replaces_snapshot_for_create_update_and_delete() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("skills");
        fs::create_dir_all(&root).unwrap();
        write_skill(&root, "demo", "demo", "first");

        let mut registry = SkillsRegistry::new();
        registry
            .register_root(registration(root.clone(), SkillRootSourceKind::UserGlobal))
            .unwrap();
        assert_eq!(registry.catalog()[0].description, "first");

        write_skill(&root, "demo", "demo", "updated");
        assert!(registry.refresh_root(&root).unwrap());
        assert_eq!(registry.catalog()[0].description, "updated");

        fs::remove_dir_all(root.join("demo")).unwrap();
        assert!(registry.refresh_root(&root).unwrap());
        assert!(registry.catalog().is_empty());
        assert!(matches!(
            registry.roots()[0].scan_status,
            SkillRootScanStatus::Scanned { .. }
        ));
    }

    #[test]
    fn rescan_rebuilds_all_registered_root_snapshots() {
        let temp = TempDir::new().unwrap();
        let user_root = temp.path().join("user");
        let agent_root = temp.path().join("agent");
        fs::create_dir_all(&user_root).unwrap();
        fs::create_dir_all(&agent_root).unwrap();
        write_skill(&user_root, "same", "same", "user");
        write_skill(&agent_root, "same", "same", "agent");

        let mut registry = SkillsRegistry::new();
        registry
            .register_root(registration(
                user_root.clone(),
                SkillRootSourceKind::UserGlobal,
            ))
            .unwrap();
        registry
            .register_root(registration(
                agent_root.clone(),
                SkillRootSourceKind::AgentHome,
            ))
            .unwrap();
        assert_eq!(registry.catalog()[0].description, "agent");

        fs::remove_dir_all(agent_root.join("same")).unwrap();
        registry.rescan();
        assert_eq!(registry.catalog()[0].description, "user");
    }

    #[test]
    fn replace_roots_removes_stale_root_entries() {
        let temp = TempDir::new().unwrap();
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        write_skill(&first_root, "first", "first", "old");
        write_skill(&second_root, "second", "second", "new");

        let mut registry = SkillsRegistry::new();
        registry
            .replace_roots(vec![registration(
                first_root.clone(),
                SkillRootSourceKind::Workspace,
            )])
            .unwrap();
        assert_eq!(registry.catalog()[0].name, "first");

        registry
            .replace_roots(vec![registration(
                second_root.clone(),
                SkillRootSourceKind::Workspace,
            )])
            .unwrap();
        let catalog = registry.catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "second");
        assert_eq!(registry.roots().len(), 1);
        assert_eq!(registry.roots()[0].root_path, second_root);
    }

    #[test]
    fn watcher_failure_is_recorded_without_changing_catalog() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("skills");
        fs::create_dir_all(&root).unwrap();
        write_skill(&root, "demo", "demo", "available");

        let mut registry = SkillsRegistry::new();
        registry
            .register_root(registration(root.clone(), SkillRootSourceKind::UserGlobal))
            .unwrap();

        assert!(registry.record_watcher_started(&root));
        assert!(registry.record_watcher_failure(&root, "backend unavailable"));
        assert_eq!(registry.catalog()[0].description, "available");
        assert!(matches!(
            registry.roots()[0].watch_status,
            SkillRootWatchStatus::Failed { .. }
        ));
    }
}
