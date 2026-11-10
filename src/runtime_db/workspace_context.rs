//! Current workspace material shared by the loaded runtimes of one database.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{RwLock, TryLockError};

use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, Transaction};

use crate::system::ExecutionRootRef;

use super::blocking::at_blocking_boundary;
use super::repositories::{decode_execution_root_entry_payload, decode_workspace_entry_payload};
use super::{persistence_certainty, PersistenceCertainty, RuntimeDb, RuntimeDbRetryableError};

type Observed<T> = std::result::Result<T, String>;
type ObservedRoots = Observed<Vec<ExecutionRootRef>>;

pub(crate) struct WorkspaceContext {
    anchors: BTreeMap<String, Observed<PathBuf>>,
    roots: BTreeMap<String, ObservedRoots>,
}

#[derive(Default)]
pub(super) enum WorkspaceContextPublication {
    #[default]
    Uninitialized,
    Ready(WorkspaceContext),
    NeedsReload,
}

pub(super) enum WorkspaceContextChange {
    Anchor(String, PathBuf),
    Roots(Vec<(String, Option<ObservedRoots>)>),
}

impl WorkspaceContext {
    fn load(connection: &Connection) -> Result<Self> {
        let mut anchors = BTreeMap::new();
        let mut statement =
            connection.prepare("SELECT workspace_id, payload_json FROM workspace_entries")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let workspace_id: String = row.get(0)?;
            let anchor = (|| {
                let entry = decode_workspace_entry_payload(row.get_ref(1)?.as_str()?)?;
                ensure!(
                    entry.workspace_id == workspace_id,
                    "context workspace projection mismatch"
                );
                Ok(entry.workspace_anchor)
            })();
            anchors.insert(
                workspace_id,
                anchor.map_err(|error: anyhow::Error| error.to_string()),
            );
        }
        Ok(Self {
            anchors,
            roots: load_roots(connection, None)?,
        })
    }

    pub(crate) fn anchor(&self, workspace_id: &str) -> Result<Option<&Path>> {
        self.anchors
            .get(workspace_id)
            .map(|anchor| {
                anchor
                    .as_ref()
                    .map(PathBuf::as_path)
                    .map_err(|error| anyhow::anyhow!("{error}"))
            })
            .transpose()
    }

    pub(crate) fn roots(&self, workspace_id: &str) -> Result<&[ExecutionRootRef]> {
        self.roots.get(workspace_id).map_or(Ok(&[]), |roots| {
            roots
                .as_ref()
                .map(Vec::as_slice)
                .map_err(|error| anyhow::anyhow!("{error}"))
        })
    }

    fn apply(&mut self, change: WorkspaceContextChange) {
        match change {
            WorkspaceContextChange::Anchor(workspace_id, anchor) => {
                self.anchors.insert(workspace_id, Ok(anchor));
            }
            WorkspaceContextChange::Roots(workspaces) => {
                for (workspace_id, roots) in workspaces {
                    if let Some(roots) = roots {
                        self.roots.insert(workspace_id, roots);
                    } else {
                        self.roots.remove(&workspace_id);
                    }
                }
            }
        }
    }
}

impl WorkspaceContextChange {
    pub(super) fn roots(
        tx: &Transaction<'_>,
        workspace_id: &str,
        previous_workspace_id: Option<&str>,
    ) -> Result<Self> {
        let workspaces = std::iter::once(workspace_id)
            .chain(previous_workspace_id.filter(|id| *id != workspace_id))
            .map(|id| Ok((id.to_owned(), load_roots(tx, Some(id))?.remove(id))))
            .collect::<Result<_>>()?;
        Ok(Self::Roots(workspaces))
    }
}

fn load_roots(
    connection: &Connection,
    workspace_id: Option<&str>,
) -> Result<BTreeMap<String, ObservedRoots>> {
    let sql = if workspace_id.is_some() {
        "SELECT workspace_id, payload_json FROM execution_root_entries
         WHERE workspace_id = ?1 AND removed_at IS NULL
         ORDER BY created_at ASC, rowid ASC"
    } else {
        "SELECT workspace_id, payload_json FROM execution_root_entries
         WHERE removed_at IS NULL
         ORDER BY workspace_id, created_at ASC, rowid ASC"
    };
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(workspace_id))?;
    let mut roots = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let workspace_id: String = row.get(0)?;
        let view = roots
            .entry(workspace_id.clone())
            .or_insert_with(|| Ok(Vec::new()));
        let Ok(refs) = view else {
            continue;
        };
        let root: Result<_> = (|| {
            let entry = decode_execution_root_entry_payload(row.get_ref(1)?.as_str()?)?;
            ensure!(
                entry.workspace_id == workspace_id && entry.removed_at.is_none(),
                "context execution root workspace projection mismatch"
            );
            Ok(ExecutionRootRef {
                execution_root_id: entry.execution_root_id,
                workspace_id: entry.workspace_id,
                filesystem_path: entry.filesystem_path,
            })
        })();
        match root {
            Ok(root) => refs.push(root),
            Err(error) => *view = Err(error.to_string()),
        }
    }
    Ok(roots)
}

impl RuntimeDb {
    fn workspace_context_publication(&self) -> &RwLock<WorkspaceContextPublication> {
        &self.writer.state.workspace_context
    }

    pub(crate) fn prepare_workspace_context(&self) -> Result<()> {
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

    /// The callback only copies the selected material, with no I/O or owner acquisition.
    pub(crate) fn with_workspace_context<T>(
        &self,
        capture: impl FnOnce(&WorkspaceContext) -> Result<T>,
    ) -> Result<T> {
        let slot = self.workspace_context_publication();
        let publication = match slot.try_read() {
            Ok(publication) => publication,
            Err(TryLockError::WouldBlock) => at_blocking_boundary(|| slot.read())
                .ok()
                .context("workspace context publication poisoned")?,
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
}
