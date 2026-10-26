//! Task-only monitor adapter. Delivery and protocol evidence retain their old adapters.
use super::*;
use crate::runtime::{TaskParentTurnRead, TaskParentTurnSession};

pub(super) enum InvocationTaskRows {
    Loaded(TaskParentTurnRead),
    Cold(BTreeMap<String, TaskStatus>),
}
impl InvocationTaskRows {
    pub(super) fn is_empty(&self) -> bool {
        self.ids().next().is_none()
    }
    pub(super) fn has_active(&self) -> bool {
        match self {
            Self::Loaded(read) => read.headers().values().any(|row| {
                matches!(
                    row.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
            }),
            Self::Cold(rows) => rows.values().any(|status| {
                matches!(
                    status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
            }),
        }
    }
    pub(super) fn contains(&self, id: &str) -> bool {
        match self {
            Self::Loaded(read) => read.headers().contains_key(id),
            Self::Cold(rows) => rows.contains_key(id),
        }
    }
    pub(super) fn ids(&self) -> impl Iterator<Item = &str> {
        let (loaded, cold) = match self {
            Self::Loaded(read) => (Some(read.headers()), None),
            Self::Cold(rows) => (None, Some(rows)),
        };
        loaded
            .into_iter()
            .flat_map(|rows| rows.keys().map(String::as_str))
            .chain(
                cold.into_iter()
                    .flat_map(|rows| rows.keys().map(String::as_str)),
            )
    }
}

impl RuntimeHost {
    pub(super) async fn monitor_runtime(
        &self,
        child: &str,
        incarnation: u64,
    ) -> Result<Option<RuntimeHandle>> {
        self.require_cold_monitor_scope(child, incarnation)?;
        let registry = self.inner.runtimes.read().await;
        let Some(entry) = registry.agents.get(child) else {
            return Ok(None);
        };
        anyhow::ensure!(
            entry.runtime.instance_incarnation() == incarnation,
            "monitor runtime incarnation changed"
        );
        // A registered, unavailable runtime is not a genuine cold reader.
        anyhow::ensure!(entry.accepts_host_access(), "monitor runtime is not ready");
        Ok(Some(entry.runtime.clone()))
    }

    pub(super) fn require_cold_monitor_scope(&self, child: &str, incarnation: u64) -> Result<()> {
        let identity = self
            .active_agent_identity(child)
            .map_err(anyhow::Error::from)?;
        anyhow::ensure!(
            identity.incarnation == incarnation,
            "cold monitor incarnation changed"
        );
        Ok(())
    }
    pub(super) async fn invocation_task_rows(
        &self,
        child: &str,
        incarnation: u64,
        turn: &str,
        roles: &mut Vec<TaskParentTurnSession>,
    ) -> Result<InvocationTaskRows> {
        if let Some(runtime) = self.monitor_runtime(child, incarnation).await? {
            anyhow::ensure!(
                runtime.instance_incarnation() == incarnation,
                "invocation Task scope incarnation changed"
            );
            let mut position = None;
            for (index, role) in roles.iter().enumerate() {
                if role.turn_id() == turn && runtime.parent_turn_session_is_current(role).await? {
                    position = Some(index);
                    break;
                }
            }
            let index = if let Some(index) = position {
                index
            } else {
                // An actual owner replacement is a fresh explicit role. No
                // failed capture/window expansion can silently re-prepare it.
                roles.retain(|role| role.turn_id() != turn);
                let role = runtime
                    .begin_parent_turn_task_session(incarnation, turn)
                    .await?;
                roles.push(role);
                roles.len() - 1
            };
            let read = runtime
                .capture_parent_turn_tasks(incarnation, &roles[index])
                .await?;
            Ok(InvocationTaskRows::Loaded(read))
        } else {
            // Genuine unloaded inspection preserves the original canonical
            // history query; it neither activates nor invents a loaded role.
            self.require_cold_monitor_scope(child, incarnation)?;
            Ok(InvocationTaskRows::Cold(
                self.runtime_db()
                    .tasks()
                    .latest_for_agent(child, usize::MAX)?
                    .into_iter()
                    .filter(|task| {
                        task.detail
                            .as_ref()
                            .and_then(|detail| detail.get("parent_turn_id"))
                            .and_then(Value::as_str)
                            == Some(turn)
                    })
                    .map(|task| (task.id, task.status))
                    .collect(),
            ))
        }
    }
}
