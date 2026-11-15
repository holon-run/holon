//! SQLite-backed workspace registry material and shared publication state.

use std::{collections::BTreeMap, path::Path};

use anyhow::{ensure, Context, Result};
use holon_domain::execution_types::ExecutionRootRef;
use holon_domain::{ExecutionRootEntry, WorkspaceEntry};
use rusqlite::{Connection, Transaction};

type Observed<T> = std::result::Result<T, String>;
type ObservedRoots = Observed<Vec<ExecutionRootRef>>;

pub struct WorkspaceContext {
    entries: BTreeMap<String, Observed<WorkspaceEntry>>,
    roots: BTreeMap<String, ObservedRoots>,
}

#[derive(Default)]
pub enum WorkspaceContextPublication {
    #[default]
    Uninitialized,
    Ready(WorkspaceContext),
    NeedsReload,
}

pub enum WorkspaceContextChange {
    Entry(WorkspaceEntry),
    Roots(Vec<(String, Option<ObservedRoots>)>),
}

impl WorkspaceContext {
    pub fn load(connection: &Connection) -> Result<Self> {
        let mut entries = BTreeMap::new();
        let mut statement =
            connection.prepare("SELECT workspace_id, payload_json FROM workspace_entries")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let workspace_id: String = row.get(0)?;
            let entry = (|| {
                let entry = decode_workspace_entry_payload(row.get_ref(1)?.as_str()?)?;
                ensure!(
                    entry.workspace_id == workspace_id,
                    "context workspace projection mismatch"
                );
                Ok(entry)
            })();
            entries.insert(
                workspace_id,
                entry.map_err(|error: anyhow::Error| error.to_string()),
            );
        }
        Ok(Self {
            entries,
            roots: load_roots(connection, None)?,
        })
    }

    pub fn anchor(&self, workspace_id: &str) -> Result<Option<&Path>> {
        Ok(self
            .entry(workspace_id)?
            .map(|entry| entry.workspace_anchor.as_path()))
    }

    pub fn entry(&self, workspace_id: &str) -> Result<Option<&WorkspaceEntry>> {
        self.entries
            .get(workspace_id)
            .map(|entry| entry.as_ref().map_err(|error| anyhow::anyhow!("{error}")))
            .transpose()
    }

    pub fn roots(&self, workspace_id: &str) -> Result<&[ExecutionRootRef]> {
        self.roots.get(workspace_id).map_or(Ok(&[]), |roots| {
            roots
                .as_ref()
                .map(Vec::as_slice)
                .map_err(|error| anyhow::anyhow!("{error}"))
        })
    }

    pub fn apply(&mut self, change: WorkspaceContextChange) {
        match change {
            WorkspaceContextChange::Entry(entry) => {
                self.entries.insert(entry.workspace_id.clone(), Ok(entry));
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
    pub fn roots(
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

pub fn decode_workspace_entry_payload(payload: &str) -> Result<WorkspaceEntry> {
    serde_json::from_str(payload).context("decoding workspace entry payload from runtime db")
}

pub fn decode_execution_root_entry_payload(payload: &str) -> Result<ExecutionRootEntry> {
    serde_json::from_str(payload).context("decoding execution root entry payload from runtime db")
}
