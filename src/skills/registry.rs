//! Skills registry: skill root registration and catalog management.
//!
//! This module implements a registry for skill discovery across multiple roots,
//! supporting user-global, agent-local, and workspace-local skill sources.
//! It maintains registration metadata and provides catalog views with scope filtering.

use anyhow::Result;
use chrono::Utc;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};
use tracing::warn;

use crate::types::{
    SkillCatalogEntry, SkillRootRegistration, SkillRootScanStatus, SkillRootSourceKind,
    SkillRootWatchStatus, SkillScope,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootInputFingerprint {
    source_kind: SkillRootSourceKind,
    owner_agent_id: Option<String>,
    children: Vec<RootChildFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RootChildFingerprint {
    path: PathBuf,
    kind: RootChildKind,
    skill_file: Option<FileMetadataFingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootChildKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileMetadataFingerprint {
    len: u64,
    modified_ns: Option<u128>,
}

/// Read-only skills registry.
///
/// The registry maintains skill entries from registered roots and provides
/// a catalog view with precedence resolution (workspace > agent > user).
#[derive(Debug, Clone, Default)]
pub struct SkillsRegistry {
    roots: Arc<Vec<SkillRootRegistration>>,
    entries: Arc<Vec<SkillCatalogEntry>>,
    root_fingerprints: Arc<BTreeMap<PathBuf, RootInputFingerprint>>,
    material: Option<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

#[cfg(test)]
impl PartialEq for SkillsRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.roots == other.roots
            && self.entries == other.entries
            && self.root_fingerprints == other.root_fingerprints
    }
}
#[cfg(test)]
impl Eq for SkillsRegistry {}

struct RootScanCandidate {
    registration: SkillRootRegistration,
    entries: Vec<SkillCatalogEntry>,
    fingerprint: Option<RootInputFingerprint>,
}

impl SkillsRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn same_snapshot(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.roots, &other.roots)
            && Arc::ptr_eq(&self.entries, &other.entries)
            && Arc::ptr_eq(&self.root_fingerprints, &other.root_fingerprints)
    }

    /// Admit COW copies before any candidate mutation, outside the inventory lock.
    pub(crate) fn admit_candidate(&self) -> Result<()> {
        use crate::runtime::input_evidence_budget::serialized_bytes;
        let mut bytes = serialized_bytes(self.roots.as_ref())?
            .checked_add(serialized_bytes(self.entries.as_ref())?)
            .and_then(|bytes| bytes.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("skill snapshot material overflow"))?;
        let mut records = self.roots.len() + self.entries.len() * 8 + 16;
        for (path, fingerprint) in self.root_fingerprints.iter() {
            bytes = bytes
                .checked_add(path.as_os_str().len() as u64 * 4 + 512)
                .ok_or_else(|| anyhow::anyhow!("skill fingerprint material overflow"))?;
            for child in &fingerprint.children {
                bytes = bytes
                    .checked_add(child.path.as_os_str().len() as u64 * 4 + 512)
                    .ok_or_else(|| anyhow::anyhow!("skill fingerprint material overflow"))?;
                records += 8;
            }
        }
        crate::diagnostics::resource_io::charge_owned_metadata(bytes + 4096, records)?;
        Ok(())
    }
    pub(crate) fn retain_material(
        &mut self,
        material: Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    ) {
        self.material = Some(material);
    }

    /// Register a skill root and scan it for skills.
    pub fn register_root(&mut self, registration: SkillRootRegistration) -> Result<()> {
        self.sync_effective_roots(std::iter::once(registration))
    }

    fn prepare_registration(
        &self,
        registration: SkillRootRegistration,
    ) -> Result<Option<RootScanCandidate>> {
        crate::diagnostics::resource_io::check_active()?;
        if !registration.root_path.try_exists()? {
            return Ok(None);
        }

        let root_key = normalize_path(&registration.root_path);
        let existing_index = self.root_index(&registration.root_path);
        let registration_changed = existing_index.is_some_and(|root_index| {
            !same_root_registration(&self.roots[root_index], &registration)
        });
        let input_fingerprint = Self::root_input_fingerprint(&registration)?;
        crate::diagnostics::resource_io::check_active()?;
        let input_changed = match (
            self.root_fingerprints.get(&root_key),
            input_fingerprint.as_ref(),
        ) {
            (Some(previous), Some(current)) => previous != current,
            _ => true,
        };

        if registration_changed || input_changed {
            return Self::prepare_root_scan(registration, input_fingerprint).map(Some);
        }
        Ok(None)
    }

    /// Synchronize lifecycle-owned effective roots into the registry snapshot.
    ///
    /// This is intentionally additive: lifecycle transitions register or refresh
    /// the roots that are effective for an agent/workspace, while catalog readers
    /// select from the existing snapshot without clearing roots owned by other
    /// agents or workspaces.
    pub fn sync_effective_roots(
        &mut self,
        registrations: impl IntoIterator<Item = SkillRootRegistration>,
    ) -> Result<()> {
        let mut candidates: Vec<RootScanCandidate> = Vec::new();
        for registration in registrations {
            // An effective set may name one path as both agent and workspace
            // scope. Its last registration wins, even when unchanged from live.
            candidates.retain(|candidate| {
                normalize_path(&candidate.registration.root_path)
                    != normalize_path(&registration.root_path)
            });
            let candidate = self.prepare_registration(registration);
            crate::diagnostics::resource_io::check_active()?;
            if let Some(candidate) = candidate? {
                candidates.push(candidate);
            }
        }
        // No shared root, entry or fingerprint changes until every scan is admitted.
        crate::diagnostics::resource_io::check_active()?;
        for candidate in candidates {
            self.install_root_scan(candidate);
        }
        Ok(())
    }

    /// Replace the registered roots with the provided effective set.
    ///
    /// This keeps shared registries scoped to the current caller's effective
    /// roots instead of accumulating stale roots from other agents or workspaces.
    pub fn replace_roots(&mut self, registrations: Vec<SkillRootRegistration>) -> Result<()> {
        Arc::make_mut(&mut self.roots).clear();
        Arc::make_mut(&mut self.entries).clear();
        Arc::make_mut(&mut self.root_fingerprints).clear();
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
    /// Ordinary scan failures remove stale entries; a resource quota failure
    /// rejects the candidate without changing the shared root snapshot.
    pub fn refresh_root(&mut self, root_path: &Path) -> Result<bool> {
        let Some(root_index) = self.root_index(root_path) else {
            return Ok(false);
        };
        let registration = self.roots[root_index].clone();
        let input_fingerprint_before = Self::root_input_fingerprint(&registration);
        crate::diagnostics::resource_io::check_active()?;
        let candidate = Self::prepare_root_scan(registration, input_fingerprint_before?);
        crate::diagnostics::resource_io::check_active()?;
        let candidate = candidate?;
        self.install_root_scan(candidate);
        Ok(true)
    }

    fn prepare_root_scan(
        mut registration: SkillRootRegistration,
        input_fingerprint_before: Option<RootInputFingerprint>,
    ) -> Result<RootScanCandidate> {
        crate::diagnostics::resource_io::charge_owned_metadata(
            std::mem::size_of::<RootScanCandidate>() as u64,
            1,
        )?;
        let (entries, scan_status) = Self::scan_root(&registration);
        crate::diagnostics::resource_io::check_active()?;
        let input_fingerprint_after = Self::root_input_fingerprint(&registration)?;
        crate::diagnostics::resource_io::check_active()?;
        registration.scan_status = scan_status;
        Ok(RootScanCandidate {
            registration,
            entries,
            fingerprint: if input_fingerprint_before == input_fingerprint_after {
                input_fingerprint_after
            } else {
                None
            },
        })
    }

    fn install_root_scan(&mut self, candidate: RootScanCandidate) {
        let RootScanCandidate {
            mut registration,
            entries,
            fingerprint,
        } = candidate;
        Arc::make_mut(&mut self.entries)
            .retain(|entry| !entry.path.starts_with(&registration.root_path));
        Arc::make_mut(&mut self.entries).extend(entries);
        let root_key = normalize_path(&registration.root_path);
        if let Some(index) = self.root_index(&registration.root_path) {
            if same_root_registration(&self.roots[index], &registration) {
                std::mem::swap(
                    &mut registration.watch_status,
                    &mut Arc::make_mut(&mut self.roots)[index].watch_status,
                );
            }
        }
        self.upsert_root(registration);
        if let Some(fingerprint) = fingerprint {
            Arc::make_mut(&mut self.root_fingerprints).insert(root_key, fingerprint);
        } else {
            Arc::make_mut(&mut self.root_fingerprints).remove(&root_key);
        }
    }

    /// Rescan all registered roots and replace the complete registry snapshot.
    ///
    /// Per-root scan failures are recorded on the root status and do not make
    /// catalog reads fail.
    pub fn rescan(&mut self) {
        let mut next_entries = Vec::new();
        Arc::make_mut(&mut self.root_fingerprints).clear();
        for root in Arc::make_mut(&mut self.roots) {
            let input_fingerprint_before = Self::root_input_fingerprint(root).ok().flatten();
            let (entries, scan_status) = Self::scan_root(root);
            let input_fingerprint_after = Self::root_input_fingerprint(root).ok().flatten();
            next_entries.extend(entries);
            root.scan_status = scan_status;
            if input_fingerprint_before == input_fingerprint_after {
                if let Some(fingerprint) = input_fingerprint_after {
                    Arc::make_mut(&mut self.root_fingerprints)
                        .insert(normalize_path(&root.root_path), fingerprint);
                }
            }
        }
        self.entries = Arc::new(next_entries);
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
        Arc::make_mut(&mut self.roots)[root_index].watch_status =
            SkillRootWatchStatus::Failed { error };
        true
    }

    /// Mark a registered root as actively watched.
    pub fn record_watcher_started(&mut self, root_path: impl AsRef<Path>) -> bool {
        let Some(root_index) = self.root_index(root_path.as_ref()) else {
            return false;
        };
        Arc::make_mut(&mut self.roots)[root_index].watch_status = SkillRootWatchStatus::Watching;
        true
    }

    fn upsert_root(&mut self, registration: SkillRootRegistration) {
        if let Some(root_index) = self.root_index(&registration.root_path) {
            Arc::make_mut(&mut self.roots)[root_index] = registration;
        } else {
            Arc::make_mut(&mut self.roots).push(registration);
        }
    }

    fn root_index(&self, root_path: &Path) -> Option<usize> {
        let root_path = normalize_path(root_path);
        self.roots
            .iter()
            .position(|root| normalize_path(&root.root_path) == root_path)
    }

    fn root_input_fingerprint(
        registration: &SkillRootRegistration,
    ) -> Result<Option<RootInputFingerprint>> {
        let mut children = Vec::new();
        let entries = match crate::diagnostics::io_probe::read_dir(&registration.root_path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        for child in entries {
            let child = child?;
            let child_path = child.path();
            let file_type = child.file_type()?;
            let kind = if file_type.is_file() {
                RootChildKind::File
            } else if file_type.is_dir() {
                RootChildKind::Directory
            } else if file_type.is_symlink() {
                RootChildKind::Symlink
            } else {
                RootChildKind::Other
            };
            let skill_file = if matches!(kind, RootChildKind::Directory | RootChildKind::Symlink) {
                skill_file_fingerprint(&child_path.join("SKILL.md"))?
            } else {
                None
            };
            crate::diagnostics::resource_io::charge_owned_metadata(512, 8)?;
            children.push(RootChildFingerprint {
                path: child_path,
                kind,
                skill_file,
            });
        }
        children.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Some(RootInputFingerprint {
            source_kind: registration.source_kind.clone(),
            owner_agent_id: registration.owner_agent_id.clone(),
            children,
        }))
    }

    fn scan_root(
        registration: &SkillRootRegistration,
    ) -> (Vec<SkillCatalogEntry>, SkillRootScanStatus) {
        let scope = match registration.source_kind {
            SkillRootSourceKind::UserGlobal => SkillScope::UserGlobal,
            SkillRootSourceKind::AgentHome => SkillScope::Agent,
            SkillRootSourceKind::Workspace => SkillScope::Workspace,
        };

        match crate::skills::load_catalog_for_root(
            scope,
            &registration.root_path,
            crate::skills::skill_root_id(registration),
        ) {
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
        self.catalog_from_entries(self.entries.iter(), scope)
    }

    /// Get catalog for a caller's effective roots without mutating the registry.
    pub fn catalog_for_roots(
        &self,
        roots: &[SkillRootRegistration],
        scope: Option<SkillScope>,
    ) -> Vec<SkillCatalogEntry> {
        self.catalog_from_entries(
            self.entries.iter().filter(|entry| {
                roots
                    .iter()
                    .any(|root| entry.path.starts_with(&root.root_path))
            }),
            scope,
        )
    }

    fn catalog_from_entries<'a>(
        &self,
        entries: impl Iterator<Item = &'a SkillCatalogEntry>,
        scope: Option<SkillScope>,
    ) -> Vec<SkillCatalogEntry> {
        let selected_by_name: BTreeMap<String, &SkillCatalogEntry> = entries
            .filter(|entry| scope.is_none_or(|filter_scope| entry.scope == filter_scope))
            .fold(BTreeMap::new(), |mut acc, entry| {
                let existing = acc.get(&entry.name);
                if existing.map_or(true, |existing| {
                    crate::skills::skill_wins_catalog_selection(entry, existing)
                }) {
                    if crate::diagnostics::resource_io::charge_owned_metadata(
                        entry.name.len() as u64
                            + std::mem::size_of::<(String, &SkillCatalogEntry)>() as u64,
                        4,
                    )
                    .is_err()
                    {
                        return acc;
                    }
                    acc.insert(entry.name.clone(), entry);
                }
                acc
            });
        selected_by_name
            .into_values()
            .filter_map(|entry| {
                crate::diagnostics::resource_io::charge_skill_catalog_entry(entry).ok()?;
                Some(entry.clone())
            })
            .collect()
    }

    /// Get all registered roots.
    pub fn roots(&self) -> &[SkillRootRegistration] {
        &self.roots
    }
}

fn same_root_registration(left: &SkillRootRegistration, right: &SkillRootRegistration) -> bool {
    left.source_kind == right.source_kind
        && left.owner_agent_id == right.owner_agent_id
        && normalize_path(&left.root_path) == normalize_path(&right.root_path)
}

fn skill_file_fingerprint(path: &Path) -> Result<Option<FileMetadataFingerprint>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(file_metadata_fingerprint(&metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn file_metadata_fingerprint(metadata: &fs::Metadata) -> FileMetadataFingerprint {
    FileMetadataFingerprint {
        len: metadata.len(),
        modified_ns: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos()),
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

        Arc::make_mut(&mut registry.entries).push(SkillCatalogEntry {
            skill_id: "user_skill".to_string(),
            root_id: "user_global:test-root".to_string(),
            skill_dir: "test".to_string(),
            name: "test".to_string(),
            description: "user".to_string(),
            path: PathBuf::from("/user/test"),
            scope: SkillScope::UserGlobal,
        });

        Arc::make_mut(&mut registry.entries).push(SkillCatalogEntry {
            skill_id: "agent_skill".to_string(),
            root_id: "agent_home:test-root".to_string(),
            skill_dir: "test".to_string(),
            name: "test".to_string(),
            description: "agent".to_string(),
            path: PathBuf::from("/agent/test"),
            scope: SkillScope::Agent,
        });

        Arc::make_mut(&mut registry.entries).push(SkillCatalogEntry {
            skill_id: "workspace_skill".to_string(),
            root_id: "workspace:test-root".to_string(),
            skill_dir: "test".to_string(),
            name: "test".to_string(),
            description: "workspace".to_string(),
            path: PathBuf::from("/workspace/test"),
            scope: SkillScope::Workspace,
        });

        let catalog = registry.catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].scope, SkillScope::Workspace);
        assert_eq!(catalog[0].description, "workspace");
    }

    #[test]
    fn test_catalog_with_filter() {
        let mut registry = SkillsRegistry::new();

        Arc::make_mut(&mut registry.entries).push(SkillCatalogEntry {
            skill_id: "skill1".to_string(),
            root_id: "agent_home:test-root".to_string(),
            skill_dir: "skill_a".to_string(),
            name: "skill_a".to_string(),
            description: "agent".to_string(),
            path: PathBuf::from("/a"),
            scope: SkillScope::Agent,
        });

        Arc::make_mut(&mut registry.entries).push(SkillCatalogEntry {
            skill_id: "skill2".to_string(),
            root_id: "user_global:test-root".to_string(),
            skill_dir: "skill_b".to_string(),
            name: "skill_b".to_string(),
            description: "user".to_string(),
            path: PathBuf::from("/b"),
            scope: SkillScope::UserGlobal,
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
    fn register_root_skips_unchanged_root_and_refreshes_changed_input() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("skills");
        fs::create_dir_all(&root).unwrap();
        write_skill(&root, "first", "first", "first");

        let registration = registration(root.clone(), SkillRootSourceKind::UserGlobal);
        let mut registry = SkillsRegistry::new();
        registry.register_root(registration.clone()).unwrap();
        Arc::make_mut(&mut registry.entries)[0].description = "cached".to_string();

        registry
            .sync_effective_roots([registration.clone()])
            .unwrap();
        assert_eq!(registry.catalog()[0].description, "cached");

        write_skill(&root, "second", "second", "second");
        registry.sync_effective_roots([registration]).unwrap();
        assert_eq!(registry.catalog().len(), 2);
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
    fn catalog_for_roots_selects_without_clearing_shared_registry() {
        let temp = TempDir::new().unwrap();
        let first_root = temp.path().join("first");
        let second_root = temp.path().join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        write_skill(&first_root, "first", "first", "one");
        write_skill(&second_root, "second", "second", "two");

        let first_registration = registration(first_root.clone(), SkillRootSourceKind::Workspace);
        let second_registration = registration(second_root.clone(), SkillRootSourceKind::Workspace);

        let mut registry = SkillsRegistry::new();
        registry
            .sync_effective_roots(vec![first_registration.clone(), second_registration])
            .unwrap();

        let first_catalog = registry.catalog_for_roots(&[first_registration], None);
        assert_eq!(first_catalog.len(), 1);
        assert_eq!(first_catalog[0].name, "first");
        assert_eq!(registry.catalog().len(), 2);
    }

    #[test]
    fn sync_duplicate_effective_path_keeps_the_last_registration() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("skills");
        write_skill(&root, "demo", "demo", "original");
        let agent = registration(root.clone(), SkillRootSourceKind::AgentHome);
        let workspace = registration(root.clone(), SkillRootSourceKind::Workspace);
        let mut registry = SkillsRegistry::new();
        registry
            .sync_effective_roots([agent.clone(), workspace.clone()])
            .unwrap();
        assert_eq!(registry.catalog()[0].scope, SkillScope::Workspace);
        assert!(registry.record_watcher_started(&root));
        let before = registry.clone();
        registry.sync_effective_roots([agent, workspace]).unwrap();
        assert_eq!(registry, before);
    }

    #[test]
    fn content_refresh_preserves_watching_and_failed_registration_status() {
        for failed in [false, true] {
            let temp = TempDir::new().unwrap();
            let root = temp.path().join("skills");
            write_skill(&root, "demo", "demo", "original");
            let registration = registration(root.clone(), SkillRootSourceKind::AgentHome);
            let mut registry = SkillsRegistry::new();
            registry.register_root(registration.clone()).unwrap();
            assert!(registry.record_watcher_started(&root));
            if failed {
                assert!(registry.record_watcher_failure(&root, "watch backend failed"));
            }
            let status = registry.roots()[0].watch_status.clone();
            write_skill(&root, "demo", "demo", "successfully refreshed content");
            registry.sync_effective_roots([registration]).unwrap();
            assert_eq!(
                registry.catalog()[0].description,
                "successfully refreshed content"
            );
            assert_eq!(registry.roots()[0].watch_status, status);
        }
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
