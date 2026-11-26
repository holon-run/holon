//! Current workspace material shared by the loaded runtimes of one database.

use std::sync::{RwLock, TryLockError};

use anyhow::{Context, Result};
use rusqlite::Transaction;

use holon_storage::workspace_context::WorkspaceContextPublication;
pub(crate) use holon_storage::workspace_context::{WorkspaceContext, WorkspaceContextChange};

use super::blocking::at_blocking_boundary;
use super::{persistence_certainty, PersistenceCertainty, RuntimeDb, RuntimeDbRetryableError};

impl RuntimeDb {
    fn workspace_context_publication(&self) -> &RwLock<WorkspaceContextPublication> {
        self.writer.workspace_context()
    }

    pub fn prepare_workspace_context(&self) -> Result<()> {
        at_blocking_boundary(|| {
            let slot = self.workspace_context_publication();
            let ready = matches!(
                *slot
                    .read()
                    .ok()
                    .context("workspace context publication poisoned")?,
                WorkspaceContextPublication::Ready(_)
            );
            if !ready {
                let mut publication = slot
                    .write()
                    .ok()
                    .context("workspace context publication poisoned")?;
                if !matches!(*publication, WorkspaceContextPublication::Ready(_)) {
                    *publication =
                        WorkspaceContextPublication::Ready(self.load_workspace_context()?);
                }
            }
            Ok(())
        })
    }

    fn load_workspace_context(&self) -> Result<WorkspaceContext> {
        self.read_for_preparation(|tx| WorkspaceContext::load(tx))
    }

    pub fn agent_workspace_registry_entries(
        &self,
        agent: &crate::types::AgentState,
    ) -> Result<Vec<crate::types::WorkspaceEntry>> {
        self.with_workspace_context(|context| {
            let mut entries = Vec::new();
            let mut selected = std::collections::HashSet::new();
            let workspace_ids = agent
                .active_workspace_entry
                .iter()
                .map(|entry| entry.workspace_id.as_str())
                .chain(agent.attached_workspaces.iter().map(String::as_str));
            for workspace_id in workspace_ids {
                self.check_read_cancellation()?;
                if selected.insert(workspace_id) {
                    if let Some(entry) = context.entry(workspace_id)? {
                        entries.push(entry.clone());
                    }
                }
            }
            Ok(entries)
        })
    }

    pub fn agent_secondary_workspace_list_entries(
        &self,
        agent: &crate::types::AgentState,
    ) -> Vec<crate::types::AgentListWorkspaceEntry> {
        if agent.attached_workspaces.is_empty() {
            return Vec::new();
        }
        let active_id = agent
            .active_workspace_entry
            .as_ref()
            .map(|entry| entry.workspace_id.as_str());
        let Ok(entries) = self.agent_workspace_registry_entries(agent) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter(|entry| Some(entry.workspace_id.as_str()) != active_id)
            .map(crate::types::AgentListWorkspaceEntry::from)
            .collect()
    }

    #[cfg(feature = "http-test-support")]
    pub fn check_workspace_context_for_test(&self) -> Result<()> {
        self.with_workspace_context(|_| Ok(()))
    }

    /// The callback only copies selected material; contention never waits on a writer.
    pub(crate) fn with_workspace_context<T>(
        &self,
        capture: impl FnOnce(&WorkspaceContext) -> Result<T>,
    ) -> Result<T> {
        let slot = self.workspace_context_publication();
        let publication = match slot.try_read() {
            Ok(publication) => publication,
            Err(TryLockError::WouldBlock) => {
                return Err(RuntimeDbRetryableError::new(
                    "workspace context publication",
                    self.path(),
                    "workspace registry publication is busy",
                )
                .into());
            }
            Err(TryLockError::Poisoned(_)) => {
                anyhow::bail!("workspace context publication poisoned")
            }
        };
        let WorkspaceContextPublication::Ready(current) = &*publication else {
            return Err(RuntimeDbRetryableError::new(
                "workspace context publication",
                self.path(),
                "workspace registry requires preparation",
            )
            .into());
        };
        capture(current)
    }

    #[cfg(any(all(test, holon_lib_test_shard = "c"), feature = "http-test-support"))]
    pub fn hold_workspace_context_publication_for_test(&self) -> impl Drop + '_ {
        self.workspace_context_publication().write().unwrap()
    }

    /// Only workspace writers take this gate. It spans the SQL commit and
    /// publication so no capture can observe a committed write's old view.
    pub(super) fn write_workspace_context<T>(
        &self,
        mut write: impl FnMut(&Transaction<'_>, bool) -> Result<(T, Option<WorkspaceContextChange>)>,
    ) -> Result<T> {
        at_blocking_boundary(|| {
            let mut publication = self
                .workspace_context_publication()
                .write()
                .ok()
                .context("workspace context publication poisoned")?;
            if matches!(*publication, WorkspaceContextPublication::NeedsReload) {
                *publication = WorkspaceContextPublication::Ready(self.load_workspace_context()?);
            }
            let ready = matches!(*publication, WorkspaceContextPublication::Ready(_));
            match self.transaction(|tx| write(tx, ready)) {
                Ok((value, change)) => {
                    if let (WorkspaceContextPublication::Ready(current), Some(change)) =
                        (&mut *publication, change)
                    {
                        current.apply(change);
                    }
                    Ok(value)
                }
                Err(error) => {
                    if ready
                        && persistence_certainty(&error)
                            != Some(PersistenceCertainty::DefinitelyNotCommitted)
                    {
                        *publication = WorkspaceContextPublication::NeedsReload;
                        match self.load_workspace_context() {
                            Ok(current) => {
                                *publication = WorkspaceContextPublication::Ready(current)
                            }
                            Err(reload) => {
                                tracing::warn!(error = %reload, "workspace context reload failed");
                            }
                        }
                    }
                    Err(error)
                }
            }
        })
    }
}

#[cfg(all(test, holon_lib_test_shard = "b"))]
mod tests {
    use super::*;
    use crate::{
        runtime_db::evidence::upsert_workspace_entry_tx,
        system::WorkspaceProjectionKind,
        types::{ExecutionRootEntry, WorkspaceEntry},
    };
    use chrono::{TimeDelta, Utc};

    #[test]
    fn workspace_context_recovers_a_commit_without_its_ack() -> Result<()> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        db.prepare_workspace_context()?;
        let workspace = WorkspaceEntry::new("workspace", home.path().join("committed"), None);
        let error = db
            .write_workspace_context(|tx, _| {
                upsert_workspace_entry_tx(tx, &workspace)?;
                // Simulate a durable commit whose original transaction wrapper lost its ACK.
                tx.execute_batch("COMMIT")?;
                Ok(((), None))
            })
            .unwrap_err();
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::Indeterminate)
        );
        db.with_workspace_context(|view| {
            assert_eq!(view.entry("workspace")?, Some(&workspace));
            assert_eq!(
                view.anchor("workspace")?,
                Some(workspace.workspace_anchor.as_path())
            );
            Ok(())
        })
    }

    #[test]
    fn workspace_context_preserves_commit_scope_and_rebuilds_from_disk() -> Result<()> {
        let home = tempfile::tempdir()?;
        let open = || {
            RuntimeDb::open_and_migrate(
                home.path().join("runtime.sqlite"),
                home.path().join("runtime.lock"),
            )
        };
        let db = open()?;
        let mut workspace = WorkspaceEntry::new("workspace", home.path().join("initial"), None);
        let unrelated = WorkspaceEntry::new("unrelated", home.path().join("unrelated"), None);
        db.workspace_entries().upsert(&workspace)?;
        db.workspace_entries().upsert(&unrelated)?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE workspace_entries SET payload_json='{}' WHERE workspace_id=?1",
                [&unrelated.workspace_id],
            )?;
            Ok(())
        })?;
        let mut root = ExecutionRootEntry {
            execution_root_id: "root".into(),
            workspace_id: workspace.workspace_id.clone(),
            filesystem_path: home.path().join("root"),
            root_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            worktree: None,
            created_at: Utc::now(),
            removed_at: None,
        };
        db.execution_root_entries().upsert(&root)?;
        db.prepare_workspace_context()?;
        db.with_workspace_context(|view| {
            assert_eq!(
                view.anchor("workspace")?,
                Some(workspace.workspace_anchor.as_path())
            );
            assert_eq!(
                view.roots("workspace")?[0].filesystem_path,
                root.filesystem_path
            );
            assert!(view.anchor("unrelated").is_err());
            Ok(())
        })?;

        let original = workspace.clone();
        workspace.workspace_anchor = home.path().join("updated");
        workspace.workspace_alias = Some("renamed".into());
        workspace.repo_name = Some("updated-repository".into());
        workspace.workspace_kind = Some("project".into());
        workspace.owner_agent_id = Some("another-agent".into());
        workspace.updated_at += TimeDelta::seconds(1);
        let writer = open()?;
        writer.workspace_entries().upsert(&workspace)?;
        writer.workspace_entries().upsert(&original)?;
        db.write_workspace_context::<()>(|tx, _| {
            let mut rolled_back = workspace.clone();
            rolled_back.workspace_anchor = home.path().join("rolled-back");
            upsert_workspace_entry_tx(tx, &rolled_back)?;
            anyhow::bail!("test workspace rollback");
        })
        .unwrap_err();
        db.with_workspace_context(|view| {
            assert_eq!(view.entry("workspace")?, Some(&workspace));
            assert_eq!(
                view.anchor("workspace")?,
                Some(workspace.workspace_anchor.as_path())
            );
            Ok(())
        })?;

        root.workspace_id = unrelated.workspace_id.clone();
        db.execution_root_entries().upsert(&root)?;
        db.with_workspace_context(|view| {
            assert!(view.roots("workspace")?.is_empty());
            assert_eq!(
                view.roots("unrelated")?[0].execution_root_id,
                root.execution_root_id
            );
            Ok(())
        })?;
        assert!(db
            .execution_root_entries()
            .mark_removed(&root.execution_root_id)?);
        db.execution_root_entries().upsert(&root)?;
        db.with_workspace_context(|view| {
            assert!(view.roots("unrelated")?.is_empty());
            Ok(())
        })?;
        drop((writer, db));

        let reopened = open()?;
        reopened.prepare_workspace_context()?;
        reopened.with_workspace_context(|view| {
            assert_eq!(view.entry("workspace")?, Some(&workspace));
            assert_eq!(
                view.anchor("workspace")?,
                Some(workspace.workspace_anchor.as_path())
            );
            assert!(view.roots("workspace")?.is_empty());
            assert!(view.roots("unrelated")?.is_empty());
            assert!(view.anchor("unrelated").is_err());
            Ok(())
        })
    }

    #[test]
    fn workspace_context_failed_ambiguous_reload_stays_unavailable_until_writer_repairs(
    ) -> Result<()> {
        let home = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.lock"),
        )?;
        let mut workspace = WorkspaceEntry::new("workspace", home.path().join("workspace"), None);
        db.workspace_entries().upsert(&workspace)?;
        db.prepare_workspace_context()?;
        let error = db
            .write_workspace_context(|tx, _| {
                tx.execute_batch(
                    "ALTER TABLE workspace_entries RENAME TO saved_workspace_entries; COMMIT",
                )?;
                Ok(((), None))
            })
            .unwrap_err();
        assert_eq!(
            persistence_certainty(&error),
            Some(PersistenceCertainty::Indeterminate)
        );
        let before = db.preparation_pool_stats_for_tests().checkout_attempts;
        let error = db
            .with_workspace_context(|view| Ok(view.entry("workspace")?.cloned()))
            .unwrap_err();
        assert!(error.is::<RuntimeDbRetryableError>(), "{error:#}");
        assert_eq!(
            db.preparation_pool_stats_for_tests().checkout_attempts,
            before
        );
        db.transaction(|tx| {
            tx.execute_batch("ALTER TABLE saved_workspace_entries RENAME TO workspace_entries")?;
            Ok(())
        })?;
        workspace.workspace_alias = Some("repaired-by-writer".into());
        workspace.updated_at += TimeDelta::seconds(1);
        db.workspace_entries().upsert(&workspace)?;
        db.with_workspace_context(|view| {
            assert_eq!(view.entry("workspace")?, Some(&workspace));
            Ok(())
        })
    }
}
