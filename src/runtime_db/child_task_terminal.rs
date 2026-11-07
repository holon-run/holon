//! Exact accepted child-monitor addresses; Task's original receipt remains authority.
mod monitor_handoff;
mod recovery;
pub(crate) use monitor_handoff::{
    observe_original_child_monitor_stop_handoff_tx, OriginalChildMonitorStopHandoff,
};
mod monitor_binding_recovery;
use crate::{
    runtime::{
        resident_input_evidence::InputEvidenceScope,
        resident_task::{TaskFactLocator, TaskFamily},
    },
    runtime_db::input_mutation_receipts::{
        InputMutationFamily, InputTaskDeletionResult, PreparedInputMutation,
    },
};
use anyhow::{ensure, Result};
pub(crate) use monitor_binding_recovery::{
    OriginalChildMonitorBindingReadView, OriginalChildMonitorBindingSparseRecovery,
};
pub(crate) use recovery::{
    OriginalChildTerminalReadView, OriginalChildTerminalRecovery,
    OriginalChildTerminalRecoveryMaterial,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OriginalChildTaskAddress {
    pub scope: InputEvidenceScope,
    pub admitted: TaskFactLocator,
    pub owner_epoch: u64,
    pub lease_id: String,
    pub generation_digest: String,
}
impl OriginalChildTaskAddress {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.scope == self.admitted.scope
                && !self.scope.agent_id.is_empty()
                && self.scope.identity_incarnation > 0
                && self.admitted.key.family == TaskFamily::Task
                && !self.admitted.key.id.is_empty()
                && self.admitted.revision > 0
                && self.owner_epoch > 0
                && !self.lease_id.is_empty()
                && digest(&self.admitted.content_digest)
                && digest(&self.admitted.projection_digest)
                && digest(&self.generation_digest),
            "original child-monitor address invalid"
        );
        Ok(())
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildTaskTerminalInstallMode {
    Current,
    NoState,
    Retired,
}
impl From<super::command_task_terminal::CommandTaskTerminalInstallMode>
    for ChildTaskTerminalInstallMode
{
    fn from(mode: super::command_task_terminal::CommandTaskTerminalInstallMode) -> Self {
        use super::command_task_terminal::CommandTaskTerminalInstallMode as Mode;
        match mode {
            Mode::Current => Self::Current,
            Mode::NoState => Self::NoState,
            Mode::Retired => Self::Retired,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ChildTaskTerminalResult {
    Delivered {
        message_id: String,
    },
    NoOp,
    Unavailable {
        reason: ChildTaskTerminalUnavailable,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildTaskTerminalUnavailable {
    OwnerNoState,
    ScopeRetired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildDelegationTerminalAck {
    pub delegation_id: String,
    pub before_digest: String,
    /// Actual stored canonical readback, not the requested completed DTO.
    pub stored_digest: String,
    pub source_changed: bool,
    pub audit_inserted: bool,
    /// The original durable-only audit is part of this command/whole ACK. It
    /// does not acquire wire-notification permission merely by being replayed.
    pub audit_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildTaskStatusScopeAck {
    pub scope: InputEvidenceScope,
    pub mode: ChildTaskTerminalInstallMode,
    pub base_revision: Option<u64>,
    pub revision: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildTaskTerminalAck {
    pub original: OriginalChildTaskAddress,
    pub install_mode: ChildTaskTerminalInstallMode,
    pub task_changed: bool,
    pub result: ChildTaskTerminalResult,
    pub deletion: Option<Box<InputTaskDeletionResult>>,
    pub delegation: Option<ChildDelegationTerminalAck>,
    /// Exact affected metadata scopes. Source-only members explicitly retain
    /// None revisions; this is never an instruction to construct a Task domain.
    pub status_scopes: Vec<ChildTaskStatusScopeAck>,
}
impl ChildTaskTerminalAck {
    pub(crate) fn validate(&self, prepared: &PreparedInputMutation) -> Result<()> {
        self.original.validate()?;
        ensure!(
            prepared.family() == InputMutationFamily::Task
                && prepared.scopes().contains(&self.original.scope),
            "child terminal ACK differs from its original Task receipt group"
        );
        match (&self.install_mode, &self.result) {
            (
                ChildTaskTerminalInstallMode::Current,
                ChildTaskTerminalResult::Delivered { message_id },
            ) => ensure!(
                !message_id.is_empty(),
                "child terminal delivery identity empty"
            ),
            (ChildTaskTerminalInstallMode::Current, ChildTaskTerminalResult::NoOp) => ensure!(
                !self.task_changed && self.deletion.is_none(),
                "child no-op reports business writes"
            ),
            (
                ChildTaskTerminalInstallMode::NoState,
                ChildTaskTerminalResult::Unavailable {
                    reason: ChildTaskTerminalUnavailable::OwnerNoState,
                },
            ) => ensure!(
                self.deletion.is_none(),
                "NoState child terminal cannot request identity deletion"
            ),
            (
                ChildTaskTerminalInstallMode::Retired,
                ChildTaskTerminalResult::Unavailable {
                    reason: ChildTaskTerminalUnavailable::ScopeRetired,
                },
            ) => ensure!(
                !self.task_changed && self.deletion.is_none(),
                "retired child terminal reports current Task writes"
            ),
            _ => anyhow::bail!("child terminal disposition differs from actual owner availability"),
        }
        if let Some(delegation) = &self.delegation {
            ensure!(
                !delegation.delegation_id.is_empty()
                    && !delegation.audit_id.is_empty()
                    && digest(&delegation.before_digest)
                    && digest(&delegation.stored_digest),
                "child delegation ACK address invalid"
            );
        }
        ensure!(
            self.status_scopes.windows(2).all(|pair| (
                &pair[0].scope.agent_id,
                pair[0].scope.identity_incarnation
            ) < (
                &pair[1].scope.agent_id,
                pair[1].scope.identity_incarnation
            )),
            "child terminal status scopes are not a unique sorted group"
        );
        ensure!(
            self.status_scopes.len() == prepared.scopes().len()
                && self
                    .status_scopes
                    .iter()
                    .zip(prepared.scopes())
                    .all(|(actual, expected)| &actual.scope == expected),
            "child ACK omitted an original group member"
        );
        if let Some(deletion) = &self.deletion {
            ensure!(
                deletion.identity.agent_id == deletion.job.agent_id
                    && deletion.identity.agent_id != self.original.scope.agent_id
                    && prepared
                        .scopes()
                        .iter()
                        .any(|scope| scope.agent_id == deletion.identity.agent_id
                            && scope.identity_incarnation == deletion.identity.incarnation)
                    && !deletion.job.deletion_id.is_empty()
                    && matches!(
                        deletion.identity.status,
                        crate::types::AgentRegistryStatus::Deleting
                            | crate::types::AgentRegistryStatus::Deleted
                    ),
                "child deletion ACK differs from original group"
            );
        }
        for scope in &self.status_scopes {
            ensure!(
                prepared.scopes().contains(&scope.scope),
                "child status ACK returned an unreserved scope"
            );
            match (scope.mode, scope.base_revision, scope.revision) {
                (ChildTaskTerminalInstallMode::Current, Some(before), Some(after)) => ensure!(
                    before > 0
                        && after
                            == before
                                .checked_add(1)
                                .context("child status revision overflow")?,
                    "child metadata revision was not finalized once"
                ),
                (_, None, None) => {}
                _ => anyhow::bail!("child status availability differs from actual metadata write"),
            }
        }
        Ok(())
    }
}
use anyhow::Context;

/// A read-only original monitor target, frozen before its child wait. This is
/// not permission to delete an identity or signal any current handle.
#[derive(Debug, Serialize)]
pub(crate) struct OriginalChildTaskTarget {
    original: OriginalChildTaskAddress,
    target_id: String,
    target_scope: Option<InputEvidenceScope>,
    association: ChildTargetAssociation,
    delivery_id: Option<String>,
    turn_baseline: u64,
    #[serde(skip)]
    binding_material: super::task_snapshot::TaskMaintenanceMaterial,
    #[serde(skip)]
    _material: Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ChildTargetAssociation {
    CreatedChild,
    ExistingInvocation,
    SpawnFailed,
}
impl OriginalChildTaskTarget {
    pub(crate) fn original(&self) -> &OriginalChildTaskAddress {
        &self.original
    }
    pub(crate) fn target_id(&self) -> &str {
        &self.target_id
    }
    fn delegation_target(&self) -> Option<&str> {
        (self.association != ChildTargetAssociation::SpawnFailed).then_some(self.target_id.as_str())
    }
    pub(crate) fn has_created_child_duty(&self) -> bool {
        self.association == ChildTargetAssociation::CreatedChild
    }
    pub(crate) fn target_scope(&self) -> Option<&InputEvidenceScope> {
        self.target_scope.as_ref()
    }
    pub(crate) fn binding_material_estimate(
        &self,
    ) -> super::task_snapshot::TaskMaintenanceMaterial {
        super::task_snapshot::TaskMaintenanceMaterial {
            bytes: self.binding_material.bytes,
            records: self.binding_material.records,
        }
    }
    pub(crate) fn binding_material_guards(
        &self,
    ) -> Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>> {
        self._material.clone()
    }
    pub(crate) fn binding_delivery_id(&self) -> Option<&str> {
        self.delivery_id.as_deref()
    }
    pub(crate) fn binding_turn_baseline(&self) -> u64 {
        self.turn_baseline
    }
    pub(crate) fn same_target(&self, other: &Self) -> bool {
        self.original == other.original
            && self.target_id == other.target_id
            && self.target_scope == other.target_scope
            && self.association == other.association
            && self.delivery_id == other.delivery_id
            && self.turn_baseline == other.turn_baseline
    }
}
/// Only the real accepted producer may request this finite original-target
/// observation. A missing Named identity remains explicit and grants no scope.
pub(crate) fn bind_original_child_target_tx(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::runtime::OriginalChildTaskProducer,
    target_id: &str,
    delivery_id: Option<&str>,
    turn_baseline: u64,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<std::sync::Arc<OriginalChildTaskTarget>> {
    use crate::types::{AgentKind, TaskRecoverySpec};
    use rusqlite::{params, OptionalExtension};
    source.validate_material_ledger(ledger)?;
    let view = source.source_view()?;
    let original = view.address();
    let task = view.admitted_task();
    ensure!(
        !target_id.is_empty(),
        "child monitor target identity is empty"
    );
    let mut material = super::task_snapshot::read_preparation::TaskReadMaterial::new(
        &original.scope,
        request,
        Some(ledger),
    );
    let n:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[target_id],|r|r.get(0))?;
    material.charge_ids(
        n.checked_mul(2).context("child identity copies overflow")?,
        2,
    )?;
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT agent_id,status,payload_json FROM agent_identities WHERE agent_id=?1",
            [target_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let identity=row.map(|(id,status,raw)|->Result<crate::types::AgentIdentityRecord>{
        let identity=super::repositories::decode_agent_identity_payload(&raw)?;
        let inc:Option<u64>=tx.query_row("SELECT CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END FROM agent_identities WHERE agent_id=?1",[target_id],|r|r.get(0))?;
        ensure!(id==target_id && identity.agent_id==id && inc==Some(identity.incarnation)
            && serde_json::to_value(identity.status)?.as_str()==Some(status.as_str()),"child target identity normalized projection differs");
        Ok(identity)
    }).transpose()?;
    let existing = match &task.recovery {
        Some(TaskRecoverySpec::AgentInvocation {
            target_agent_id,
            created_new_subagent,
            ..
        }) => {
            let expected = target_agent_id
                .as_deref()
                .or_else(|| {
                    task.detail
                        .as_ref()
                        .and_then(|v| v.get("target_agent_id"))
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| {
                    task.detail
                        .as_ref()
                        .and_then(|v| v.get("child_agent_id"))
                        .and_then(serde_json::Value::as_str)
                });
            let actual_created = *created_new_subagent
                && identity.as_ref().is_some_and(|child| {
                    child.kind == AgentKind::Child
                        && child.parent_agent_id.as_deref()
                            == Some(original.scope.agent_id.as_str())
                        && child.delegated_from_task_id.as_deref() == Some(task.id.as_str())
                });
            if actual_created {
                ensure!(
                    expected.is_none_or(|id| id == target_id),
                    "new invocation changed original target"
                );
                false
            } else {
                ensure!(
                    expected == Some(target_id),
                    "invocation changed its original target"
                );
                true
            }
        }
        _ => false,
    };
    let association = if existing {
        if let Some(delivery_id) = delivery_id {
            let n:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(target_agent_id AS BLOB))+length(CAST(delivery_id AS BLOB))+length(CAST(outcome AS BLOB)) FROM agent_message_deliveries WHERE delivery_id=?1),0)",[delivery_id],|r|r.get(0))?;
            material.charge_ids(
                n.checked_mul(2).context("child delivery copies overflow")?,
                2,
            )?;
            let actual:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_message_deliveries WHERE delivery_id=?1 AND target_agent_id=?2 AND outcome='accepted' AND json_extract(payload_json,'$.delivery_id')=delivery_id AND json_extract(payload_json,'$.target_agent_id')=target_agent_id AND json_extract(payload_json,'$.outcome')=outcome AND json_extract(payload_json,'$.caller.current_task_id')=?3 AND json_extract(payload_json,'$.caller.caller_agent_id')=?4)",params![delivery_id,target_id,task.id,original.scope.agent_id],|r|r.get(0))?;
            ensure!(
                actual,
                "child monitor delivery differs from its original invocation"
            );
        }
        ChildTargetAssociation::ExistingInvocation
    } else {
        let child = identity
            .as_ref()
            .context("original created child identity is missing")?;
        ensure!(
            child.kind == AgentKind::Child
                && child.parent_agent_id.as_deref() == Some(original.scope.agent_id.as_str())
                && child.delegated_from_task_id.as_deref() == Some(task.id.as_str()),
            "original created child parent/task association differs"
        );
        ensure!(
            delivery_id.is_none(),
            "new child monitor acquired an unrelated invocation delivery"
        );
        ChildTargetAssociation::CreatedChild
    };
    let address_bytes = crate::runtime::input_evidence_budget::serialized_bytes(&(
        original,
        target_id,
        delivery_id,
        turn_baseline,
    ))?;
    material.charge_ids(
        usize::try_from(address_bytes)?
            .checked_add(512)
            .context("child target address overflow")?,
        8,
    )?;
    Ok(std::sync::Arc::new(OriginalChildTaskTarget {
        original: original.clone(),
        target_id: target_id.into(),
        target_scope: identity.as_ref().map(|identity| InputEvidenceScope {
            agent_id: identity.agent_id.clone(),
            identity_incarnation: identity.incarnation,
        }),
        association,
        delivery_id: delivery_id.map(str::to_owned),
        turn_baseline,
        binding_material: material.material_estimate(),
        _material: material.into_guards(),
    }))
}

/// Complete finite dependency discovery. These locators are selectors only;
/// their owners and exact aliases must be revalidated in the business TX.
#[derive(Debug, Serialize)]
pub(crate) struct ChildTaskObserverInventory {
    original: OriginalChildTaskAddress,
    target_id: Option<String>,
    rows: Vec<ChildTaskObserver>,
    #[serde(skip)]
    _material: Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    #[serde(skip)]
    material_bytes: usize,
    #[serde(skip)]
    material_records: usize,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ChildTaskObserver {
    pub(crate) task: TaskFactLocator,
}
impl ChildTaskObserverInventory {
    pub(crate) fn material_estimate(&self) -> super::task_snapshot::TaskMaintenanceMaterial {
        super::task_snapshot::TaskMaintenanceMaterial {
            bytes: self.material_bytes,
            records: self.material_records,
        }
    }
    pub(crate) fn rows(&self) -> &[ChildTaskObserver] {
        &self.rows
    }
    pub(crate) fn original(&self) -> &OriginalChildTaskAddress {
        &self.original
    }
    pub(crate) fn validate_complete_tx(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
        use rusqlite::params;
        let Some(target_id) = self.target_id.as_deref() else {
            ensure!(
                self.rows.is_empty(),
                "targetless child inventory has dependent scopes"
            );
            return Ok(());
        };
        let count: usize = tx.query_row(
            "SELECT COUNT(*) FROM task_status_child_observers WHERE child_agent_id=?1",
            [target_id],
            |r| r.get(0),
        )?;
        ensure!(count == self.rows.len(), ChildObserverSetChanged);
        for row in &self.rows {
            let fact = &row.task;
            let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_status_child_observers WHERE agent_id=?1 AND identity_incarnation=?2 AND task_id=?3 AND child_agent_id=?4 AND task_revision=?5 AND content_digest=?6 AND projection_digest=?7)",params![fact.scope.agent_id,fact.scope.identity_incarnation,fact.key.id,target_id,fact.revision,fact.content_digest,fact.projection_digest],|r|r.get(0))?;
            ensure!(exact, ChildObserverSetChanged);
        }
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
#[error("original child Status observer group changed before business SQL")]
pub(crate) struct ChildObserverSetChanged;

pub(crate) fn discover_original_child_observers_tx(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::runtime::OriginalChildTaskProducer,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<std::sync::Arc<ChildTaskObserverInventory>> {
    use rusqlite::params;
    source.validate_material_ledger(ledger)?;
    let view = source.source_view()?;
    let Some(target) = view
        .target()
        .filter(|target| target.delegation_target().is_some())
    else {
        let original = view.address();
        let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
            original,
        )?)?
        .checked_add(256)
        .and_then(|n| n.checked_mul(8))
        .context("targetless child inventory overflow")?;
        let guard = super::command_task_terminal::charge_material(
            &original.scope,
            bytes,
            8,
            request,
            ledger,
        )?;
        return Ok(std::sync::Arc::new(ChildTaskObserverInventory {
            original: original.clone(),
            target_id: None,
            rows: Vec::new(),
            _material: vec![guard],
            material_bytes: bytes,
            material_records: 8,
        }));
    };
    ensure!(
        target.original() == view.address(),
        "child observer discovery crossed original target"
    );
    let original = view.address();
    let child = target.target_id();
    let mut material = super::task_snapshot::read_preparation::TaskReadMaterial::new(
        &original.scope,
        request,
        Some(ledger),
    );
    let (count,bytes):(usize,usize)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(agent_id AS BLOB))+length(CAST(task_id AS BLOB))+length(CAST(child_agent_id AS BLOB))+length(CAST(content_digest AS BLOB))+length(CAST(projection_digest AS BLOB))+256),0) FROM task_status_child_observers WHERE child_agent_id=?1",[child],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let address = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(&(
        original, child,
    ))?)?;
    material.charge_ids(
        bytes
            .checked_add(address)
            .and_then(|n| n.checked_add(512))
            .context("child observer material overflow")?,
        count
            .checked_mul(10)
            .and_then(|n| n.checked_add(8))
            .context("child observer count overflow")?,
    )?;
    // Native validation is integer-only; a damaged digest cannot allocate a
    // large String before the byte/count gate above.
    let invalid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM task_status_child_observers WHERE child_agent_id=?1 AND (identity_incarnation<=0 OR task_revision<=0 OR length(content_digest)!=64 OR content_digest GLOB '*[^0-9a-fA-F]*' OR length(projection_digest)!=64 OR projection_digest GLOB '*[^0-9a-fA-F]*'))",[child],|r|r.get(0))?;
    ensure!(
        !invalid,
        "child observer index contains an invalid original locator"
    );
    let mut query=tx.prepare("SELECT agent_id,identity_incarnation,task_id,task_revision,content_digest,projection_digest FROM task_status_child_observers WHERE child_agent_id=?1 ORDER BY agent_id,identity_incarnation,task_id")?;
    let rows = query
        .query_map(params![child], |r| {
            Ok(ChildTaskObserver {
                task: TaskFactLocator {
                    scope: InputEvidenceScope {
                        agent_id: r.get(0)?,
                        identity_incarnation: r.get(1)?,
                    },
                    key: crate::runtime::resident_task::TaskKey::new(
                        TaskFamily::Task,
                        r.get::<_, String>(2)?,
                    ),
                    revision: r.get(3)?,
                    content_digest: r.get(4)?,
                    projection_digest: r.get(5)?,
                },
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        rows.len() == count,
        "child observer inventory changed within one view"
    );
    let usage = material.material_estimate();
    Ok(std::sync::Arc::new(ChildTaskObserverInventory {
        material_bytes: usage.bytes,
        material_records: usage.records,
        original: original.clone(),
        target_id: Some(child.into()),
        rows,
        _material: material.into_guards(),
    }))
}

/// One chosen original source body. The real producer retains this after DNC;
/// a retry may not silently switch to another latest delegation or result text.
#[derive(Debug, Serialize)]
pub(crate) struct OriginalChildDelegationChoice {
    original: OriginalChildTaskAddress,
    seed_digest: String,
    target_id: Option<String>,
    before: Option<crate::types::WorkItemDelegationRecord>,
    completed: Option<crate::types::WorkItemDelegationRecord>,
    audit: Option<crate::types::AuditEvent>,
    #[serde(skip)]
    _material: Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    #[serde(skip)]
    material_bytes: usize,
    #[serde(skip)]
    material_records: usize,
}
impl OriginalChildDelegationChoice {
    pub(crate) fn original(&self) -> &OriginalChildTaskAddress {
        &self.original
    }
    pub(crate) fn seed_digest(&self) -> &str {
        &self.seed_digest
    }
    pub(crate) fn before(&self) -> Option<&crate::types::WorkItemDelegationRecord> {
        self.before.as_ref()
    }
    pub(in crate::runtime_db) fn audit(&self) -> Option<&crate::types::AuditEvent> {
        self.audit.as_ref()
    }
    pub(crate) fn material_estimate(&self) -> super::task_snapshot::TaskMaintenanceMaterial {
        super::task_snapshot::TaskMaintenanceMaterial {
            bytes: self.material_bytes,
            records: self.material_records,
        }
    }
}
const DELEGATION_LENGTH:&str="length(CAST(payload_json AS BLOB))+length(CAST(delegation_id AS BLOB))+length(CAST(parent_agent_id AS BLOB))+length(CAST(parent_work_item_id AS BLOB))+length(CAST(child_agent_id AS BLOB))+length(CAST(child_work_item_id AS BLOB))+length(CAST(state AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(updated_at AS BLOB))+256";
fn read_delegation_on(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<crate::types::WorkItemDelegationRecord> {
    let (raw,projection):(String,String)=tx.query_row("SELECT payload_json,json_array(delegation_id,parent_agent_id,parent_work_item_id,child_agent_id,child_work_item_id,state,created_at,updated_at) FROM work_item_delegations WHERE delegation_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let record = super::repositories::decode_work_item_delegation_payload(&raw)?;
    ensure!(
        serde_json::from_str::<serde_json::Value>(&projection)?
            == serde_json::json!([
                &record.delegation_id,
                &record.parent_agent_id,
                &record.parent_work_item_id,
                &record.child_agent_id,
                &record.child_work_item_id,
                &record.state,
                super::repositories::timestamp(record.created_at),
                super::repositories::timestamp(record.updated_at)
            ]),
        "original delegation normalized projection differs"
    );
    Ok(record)
}
/// Called under the complete held observer group, before the first Task SQL
/// attempt. Only this exact real producer can freeze the global Open selector.
pub(crate) fn choose_original_child_delegation_tx(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::runtime::OriginalChildTaskProducer,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<std::sync::Arc<OriginalChildDelegationChoice>> {
    use rusqlite::OptionalExtension;
    source.validate_material_ledger(ledger)?;
    let view = source.source_view()?;
    let original = view.address();
    if let Some(choice) = source.delegation_choice()? {
        ensure!(
            choice.original() == original,
            "child delegation choice crossed original producer"
        );
        return Ok(choice);
    }
    let seed = source
        .frozen_terminal()?
        .context("child monitor has not retained its actual terminal body")?;
    view.validate_frozen(&seed)?;
    let Some(target) = view
        .target()
        .filter(|target| target.delegation_target().is_some())
    else {
        let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
            original,
        )?)?
        .checked_add(512)
        .and_then(|n| n.checked_mul(8))
        .context("targetless child source overflow")?;
        let guard = super::command_task_terminal::charge_material(
            &original.scope,
            bytes,
            16,
            request,
            ledger,
        )?;
        let choice = std::sync::Arc::new(OriginalChildDelegationChoice {
            original: original.clone(),
            seed_digest: seed.digest()?,
            target_id: None,
            before: None,
            completed: None,
            audit: None,
            _material: vec![guard],
            material_bytes: bytes,
            material_records: 16,
        });
        drop(view);
        return source.install_delegation_choice(choice);
    };
    let crate::types::MessageBody::Text { text } = &seed.message.body else {
        anyhow::bail!("child terminal source is not its original text result")
    };
    let mut material = super::task_snapshot::read_preparation::TaskReadMaterial::new(
        &original.scope,
        request,
        Some(ledger),
    );
    let size:Option<usize>=tx.query_row(&format!("SELECT {DELEGATION_LENGTH} FROM work_item_delegations WHERE child_agent_id=?1 AND state='open' ORDER BY updated_at DESC,created_at DESC,delegation_id ASC LIMIT 1"),[target.target_id()],|r|r.get(0)).optional()?;
    let before = if let Some(size) = size {
        material.charge_ids(
            size.checked_mul(2)
                .context("child delegation copies overflow")?,
            16,
        )?;
        let id:String=tx.query_row("SELECT delegation_id FROM work_item_delegations WHERE child_agent_id=?1 AND state='open' ORDER BY updated_at DESC,created_at DESC,delegation_id ASC LIMIT 1",[target.target_id()],|r|r.get(0))?;
        let record = read_delegation_on(tx, &id)?;
        ensure!(
            record.child_agent_id == target.target_id()
                && record.state == crate::types::WorkItemDelegationState::Open,
            "child delegation source changed its global Open predicate"
        );
        Some(record)
    } else {
        None
    };
    let extra = crate::runtime::input_evidence_budget::serialized_bytes(&(
        original,
        target.target_id(),
        text,
    ))?;
    material.charge_ids(
        usize::try_from(extra)?
            .checked_add(1024)
            .context("child delegation frozen material overflow")?
            .checked_mul(2)
            .context("child delegation frozen copies overflow")?,
        32,
    )?;
    // The small source structs have fixed fields. Count their owned Value nodes
    // as well as the original strings before retaining the audit clone.
    material.charge_ids(4096, 64)?;
    let completed = before
        .as_ref()
        .map(|before| crate::types::WorkItemDelegationRecord {
            state: crate::types::WorkItemDelegationState::Completed,
            result_summary: Some(text.clone()),
            updated_at: seed.at,
            ..before.clone()
        });
    let audit = completed
        .as_ref()
        .map(|after| -> Result<crate::types::AuditEvent> {
            let mut event = crate::types::AuditEvent::legacy(
                "work_item_delegation_completed",
                serde_json::to_value(after)?,
            );
            event.id = format!("audit:child-delegation:{}", original.lease_id);
            event.created_at = seed.at;
            Ok(event)
        })
        .transpose()?;
    let admitted = material.material_estimate();
    let choice = std::sync::Arc::new(OriginalChildDelegationChoice {
        original: original.clone(),
        seed_digest: seed.digest()?,
        target_id: Some(target.target_id().into()),
        before,
        completed,
        audit,
        _material: material.into_guards(),
        material_bytes: admitted.bytes,
        material_records: admitted.records,
    });
    drop(view);
    source.install_delegation_choice(choice)
}
/// Same immutable source identity, preserving the old monotone update rather
/// than imposing a new full-body OCC on another already accepted monitor.
fn same_delegation_generation(
    before: &crate::types::WorkItemDelegationRecord,
    actual: &crate::types::WorkItemDelegationRecord,
) -> bool {
    before.delegation_id == actual.delegation_id
        && before.parent_agent_id == actual.parent_agent_id
        && before.parent_work_item_id == actual.parent_work_item_id
        && before.child_agent_id == actual.child_agent_id
        && before.child_work_item_id == actual.child_work_item_id
        && before.created_at == actual.created_at
}

/// Private source capability and the same actual TX, not a DTO/child-id bool.
/// Task/Result authority and the complete owner group remain the enclosing
/// original Task facet's obligations; this function grants neither of them.
pub(in crate::runtime_db) fn apply_original_child_delegation_tx(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::runtime::OriginalChildTaskProducer,
    choice: &OriginalChildDelegationChoice,
    original_request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    material: &mut super::task_snapshot::read_preparation::TaskReadMaterial<'_>,
) -> Result<Option<ChildDelegationTerminalAck>> {
    source.validate_material_ledger(ledger)?;
    let view = source.source_view()?;
    ensure!(
        view.address() == choice.original()
            && view.target().and_then(|target| target.delegation_target())
                == choice.target_id.as_deref(),
        "delegation source crossed its original child monitor"
    );
    source.validate_terminal_digest(choice.seed_digest())?;
    material.validate_original_allowance(&choice.original.scope, original_request, ledger)?;
    let Some(before) = choice.before.as_ref() else {
        ensure!(
            choice.completed.is_none() && choice.audit.is_none(),
            "empty child delegation choice contains effects"
        );
        return Ok(None);
    };
    let completed = choice
        .completed
        .as_ref()
        .context("child delegation completion body missing")?;
    let event = choice
        .audit
        .as_ref()
        .context("child delegation original audit missing")?;
    ensure!(
        same_delegation_generation(before, completed)
            && completed.state == crate::types::WorkItemDelegationState::Completed,
        "child delegation frozen completion changed original generation"
    );
    let bytes: usize = tx.query_row(
        &format!("SELECT {DELEGATION_LENGTH} FROM work_item_delegations WHERE delegation_id=?1"),
        [&before.delegation_id],
        |r| r.get(0),
    )?;
    material.charge_ids(
        bytes
            .checked_mul(2)
            .context("child delegation current copies overflow")?,
        16,
    )?;
    let actual = read_delegation_on(tx, &before.delegation_id)?;
    ensure!(
        same_delegation_generation(before, &actual),
        "child delegation immutable original source changed"
    );
    // Original monotone SQL accepts another already completed version in the
    // same generation. Do not turn the old global selector into a new full-body
    // OCC that would strand a concurrent accepted monitor's Task terminal.
    let changed = super::repositories::upsert_work_item_delegation_with_ack_tx(tx, completed)?;
    let bytes: usize = tx.query_row(
        &format!("SELECT {DELEGATION_LENGTH} FROM work_item_delegations WHERE delegation_id=?1"),
        [&before.delegation_id],
        |r| r.get(0),
    )?;
    material.charge_ids(
        bytes
            .checked_mul(2)
            .context("child delegation stored copies overflow")?,
        16,
    )?;
    let stored = read_delegation_on(tx, &before.delegation_id)?;
    ensure!(
        same_delegation_generation(before, &stored),
        "child delegation actual ACK changed generation"
    );
    let audit_bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(data_json AS BLOB))+COALESCE(length(CAST(agent_id AS BLOB)),0) FROM audit_events WHERE audit_event_id=?1),0)+COALESCE((SELECT length(CAST(value AS BLOB)) FROM runtime_metadata WHERE key='event_log_epoch'),0)+256",[&event.id],|r|r.get(0))?;
    material.charge_ids(
        audit_bytes
            .checked_mul(2)
            .context("child delegation duplicate audit copies overflow")?,
        2,
    )?;
    let nodes:usize=tx.query_row("SELECT COALESCE((SELECT COUNT(*) FROM audit_events a,json_tree(a.data_json) WHERE a.audit_event_id=?1),0)",[&event.id],|r|r.get(0))?;
    material.charge_ids(
        nodes
            .checked_mul(256)
            .context("child delegation duplicate audit nodes overflow")?,
        nodes
            .checked_mul(2)
            .context("child delegation duplicate audit count overflow")?,
    )?;
    // This was durable-only append_event before the migration. Preserve the
    // original audit without introducing new wire effects on receipt replay.
    let (_, audit_inserted) =
        super::evidence::append_audit_event_tx(tx, Some(&choice.original.scope.agent_id), event)?;
    Ok(Some(ChildDelegationTerminalAck {
        delegation_id: before.delegation_id.clone(),
        before_digest: super::input_mutation_receipts::canonical_command_digest(before)?,
        stored_digest: super::input_mutation_receipts::canonical_command_digest(&stored)?,
        source_changed: changed,
        audit_inserted,
        audit_id: event.id.clone(),
    }))
}

/// A real Host spawn failure may precede identity creation. Its small physical
/// locator has no incarnation or body authority; it also creates no delegation.
pub(crate) fn bind_failed_original_child_target_tx(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::runtime::OriginalChildTaskProducer,
    failure: &crate::host::ChildTaskSpawnFailure,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<std::sync::Arc<OriginalChildTaskTarget>> {
    source.validate_material_ledger(ledger)?;
    let view = source.source_view()?;
    ensure!(
        view.admitted_task().kind.is_child_agent(),
        "failed spawn is not an original child Task"
    );
    let id = failure.child_agent_id();
    let n:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB))+length(CAST(payload_json AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[id],|row|row.get(0))?;
    let mut material = super::task_snapshot::read_preparation::TaskReadMaterial::new(
        &view.address().scope,
        request,
        Some(ledger),
    );
    material.charge_ids(
        n.checked_add(id.len() + 512)
            .and_then(|n| n.checked_mul(2))
            .context("failed child target material overflow")?,
        4,
    )?;
    if let Some(original) = source.original_created_spawn_scope()? {
        ensure!(
            original.agent_id == id && original.identity_incarnation > 0,
            "failed child differs from original created scope"
        );
        let matches:Option<bool>=rusqlite::OptionalExtension::optional(tx.query_row("SELECT COALESCE(agent_id=?1 AND (CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END)=?2,0) FROM agent_identities WHERE agent_id=?1",rusqlite::params![id,original.identity_incarnation],|row|row.get(0)))?;
        ensure!(
            matches == Some(true),
            "failed child identity differs from original created incarnation"
        );
    }
    let raw:Option<(String,String,String,Option<u64>)>=rusqlite::OptionalExtension::optional(tx.query_row("SELECT agent_id,status,payload_json,CASE WHEN json_type(payload_json,'$.incarnation') IS NULL THEN 1 WHEN json_type(payload_json,'$.incarnation')='integer' AND json_extract(payload_json,'$.incarnation')>0 THEN json_extract(payload_json,'$.incarnation') END FROM agent_identities WHERE agent_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))))?;
    let identity = raw
        .map(
            |(agent, status, raw, inc)| -> Result<crate::types::AgentIdentityRecord> {
                let identity = super::repositories::decode_agent_identity_payload(&raw)?;
                ensure!(
                    agent == id
                        && identity.agent_id == agent
                        && inc == Some(identity.incarnation)
                        && serde_json::to_value(identity.status)?.as_str() == Some(status.as_str()),
                    "failed child identity normalized projection differs"
                );
                Ok(identity)
            },
        )
        .transpose()?;
    let identity = identity.as_ref();
    if let Some(identity) = identity {
        ensure!(
            identity.kind == crate::types::AgentKind::Child
                && identity.parent_agent_id.as_deref()
                    == Some(view.address().scope.agent_id.as_str())
                && identity.delegated_from_task_id.as_deref()
                    == Some(view.admitted_task().id.as_str()),
            "failed child target differs from original parent/task"
        );
    }
    let target_scope = identity.map(|identity| InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    });
    let binding_material = material.material_estimate();
    let guards = material.into_guards();
    Ok(std::sync::Arc::new(OriginalChildTaskTarget {
        original: view.address().clone(),
        target_id: id.into(),
        target_scope,
        association: ChildTargetAssociation::SpawnFailed,
        delivery_id: None,
        turn_baseline: 0,
        binding_material,
        _material: guards,
    }))
}

/// The owned sorted group is accounted before copying dependency addresses.
pub(crate) struct OriginalChildObserverScopes {
    pub(crate) scopes: Vec<InputEvidenceScope>,
    pub(crate) guard: std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
    pub(crate) extra_guards:
        Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    pub(crate) material: super::task_snapshot::TaskMaintenanceMaterial,
}
pub(crate) fn reserved_observer_scopes(
    inventory: &ChildTaskObserverInventory,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<OriginalChildObserverScopes> {
    let bytes = inventory
        .rows
        .iter()
        .try_fold(
            inventory.original.scope.agent_id.len() + 256usize,
            |n, row| {
                n.checked_add(row.task.scope.agent_id.len() + 256)
                    .context("child owner group bytes overflow")
            },
        )?
        .checked_mul(8)
        .context("child owner group copies overflow")?;
    let records = inventory
        .rows
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_mul(8))
        .context("child owner group count overflow")?;
    let guard = super::command_task_terminal::charge_material(
        &inventory.original.scope,
        bytes,
        records,
        request,
        ledger,
    )?;
    let mut scopes = Vec::with_capacity(inventory.rows.len() + 1);
    scopes.push(inventory.original.scope.clone());
    scopes.extend(inventory.rows.iter().map(|row| row.task.scope.clone()));
    scopes.sort_by(|a, b| {
        (&a.agent_id, a.identity_incarnation).cmp(&(&b.agent_id, b.identity_incarnation))
    });
    scopes.dedup();
    Ok(OriginalChildObserverScopes {
        scopes,
        guard,
        extra_guards: Vec::new(),
        material: super::task_snapshot::TaskMaintenanceMaterial { bytes, records },
    })
}

/// The deletion child is a business member only while the original request is
/// still being attempted. A typed rollback fallback does not retain extra owners.
pub(crate) fn terminal_observer_scopes(
    source: &crate::runtime::OriginalChildTaskProducer,
    frozen: &crate::runtime::FrozenChildTaskTerminal,
    inventory: &ChildTaskObserverInventory,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<OriginalChildObserverScopes> {
    let mut group = reserved_observer_scopes(inventory, request, ledger)?;
    if frozen.deletion.is_some() && source.original_deletion_requested() {
        let view = source.source_view()?;
        view.validate_frozen(frozen)?;
        if let Some(target) = view.target().and_then(|target| target.target_scope()) {
            if !group.scopes.contains(target) {
                let bytes = target
                    .agent_id
                    .len()
                    .checked_add(256)
                    .and_then(|n| n.checked_mul(8))
                    .context("child business member bytes overflow")?;
                group
                    .extra_guards
                    .push(super::command_task_terminal::charge_material(
                        &source.address().scope,
                        bytes,
                        8,
                        request,
                        ledger,
                    )?);
                group.material.bytes = group
                    .material
                    .bytes
                    .checked_add(bytes)
                    .context("child business group bytes overflow")?;
                group.material.records = group
                    .material
                    .records
                    .checked_add(8)
                    .context("child business group records overflow")?;
                group.scopes.push(target.clone());
                group.scopes.sort_by(|a, b| {
                    (&a.agent_id, a.identity_incarnation)
                        .cmp(&(&b.agent_id, b.identity_incarnation))
                });
            }
        }
    }
    Ok(group)
}

/// Before borrowing in-memory facts, classify every original reserved scope.
/// Retired entries never borrow a successor Runtime or its published domains.
pub(crate) struct OriginalChildActorOwnerModes {
    rows: Vec<(InputEvidenceScope, ChildTaskTerminalInstallMode)>,
    guards: Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    material: super::task_snapshot::TaskMaintenanceMaterial,
}
impl OriginalChildActorOwnerModes {
    pub(crate) fn is_current(&self, scope: &InputEvidenceScope) -> bool {
        self.rows
            .iter()
            .any(|(s, mode)| s == scope && *mode == ChildTaskTerminalInstallMode::Current)
    }
    pub(in crate::runtime_db) fn into_material(
        self,
    ) -> (
        Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
        super::task_snapshot::TaskMaintenanceMaterial,
    ) {
        (self.guards, self.material)
    }
}
pub(crate) fn observe_original_child_actor_modes_tx(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::runtime::OriginalChildTaskProducer,
    scopes: &[(String, u64)],
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<OriginalChildActorOwnerModes> {
    source.validate_material_ledger(ledger)?;
    let mut phase = super::task_status_metadata::PhaseMaterial::new(request);
    let bytes = scopes
        .iter()
        .try_fold(256usize, |n, (id, _)| {
            n.checked_add(id.len() + 256)
                .context("child actor scope slots overflow")
        })?
        .checked_mul(8)
        .context("child actor scope copies overflow")?;
    let records = scopes
        .len()
        .checked_mul(8)
        .context("child actor scope records overflow")?;
    let mut guards = vec![super::command_task_terminal::charge_material(
        &source.address().scope,
        bytes,
        records,
        &phase.remaining()?,
        ledger,
    )?];
    phase.include(bytes, records)?;
    let mut rows = Vec::with_capacity(scopes.len());
    for (agent, incarnation) in scopes {
        let scope = InputEvidenceScope {
            agent_id: agent.clone(),
            identity_incarnation: *incarnation,
        };
        let owner = phase.owner_tx(tx, &scope, ledger)?;
        let mode = owner.mode.into();
        guards.extend(owner.into_material());
        rows.push((scope, mode));
    }
    let remaining = phase.remaining()?;
    Ok(OriginalChildActorOwnerModes {
        rows,
        guards,
        material: super::task_snapshot::TaskMaintenanceMaterial {
            bytes: request.byte_limit.min(64 * 1024 * 1024) - remaining.byte_limit,
            records: request.record_limit.min(65536) - remaining.record_limit,
        },
    })
}

/// Full Child ACK bodies have their own nested collection shape. Keep the
/// original full-column raw gate, then count all owned outcome nodes before lookup.
pub(in crate::runtime_db) fn child_receipt_material_tx(
    tx: &rusqlite::Transaction<'_>,
    prepared: &PreparedInputMutation,
    scope: &InputEvidenceScope,
    request: &super::task_snapshot::TaskDomainLoadRequest,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
) -> Result<(
    Vec<std::sync::Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
    usize,
    usize,
)> {
    let mut phase = super::task_status_metadata::PhaseMaterial::new(request);
    let bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(family AS BLOB))+length(CAST(phase_version AS BLOB))+length(CAST(command_digest AS BLOB))+length(CAST(scope_digest AS BLOB))+length(CAST(scopes_json AS BLOB))+length(CAST(prepared_at AS BLOB))+length(CAST(prepared_json AS BLOB))+length(CAST(outcome_json AS BLOB))+length(CAST(committed_at AS BLOB))+length(CAST(result_digest AS BLOB)) FROM input_mutation_receipts WHERE command_nonce=?1),0)",[prepared.command_nonce()],|row|row.get(0))?;
    let bytes = bytes
        .checked_mul(8)
        .context("Child raw receipt bytes overflow")?;
    let raw = super::command_task_terminal::charge_material(
        scope,
        bytes,
        0,
        &phase.remaining()?,
        ledger,
    )?;
    phase.include(bytes, 0)?;
    // All three independently decoded JSON objects are covered. Nothing parses
    // any JSON before the scalar full-column guard above is actually retained.
    let nodes:usize=tx.query_row("SELECT COALESCE((SELECT (SELECT COUNT(*) FROM json_tree(r.prepared_json))+(SELECT COUNT(*) FROM json_tree(r.outcome_json))+(SELECT COUNT(*) FROM json_tree(r.scopes_json)) FROM input_mutation_receipts r WHERE r.command_nonce=?1),0)",[prepared.command_nonce()],|row|row.get(0))?;
    let native_records = nodes
        .checked_mul(8)
        .context("Child receipt native nodes overflow")?;
    let native_bytes = native_records
        .checked_mul(256)
        .context("Child receipt native bytes overflow")?;
    let native = super::command_task_terminal::charge_material(
        scope,
        native_bytes,
        native_records,
        &phase.remaining()?,
        ledger,
    )?;
    phase.include(native_bytes, native_records)?;
    Ok((
        vec![raw, native],
        bytes
            .checked_add(native_bytes)
            .context("Child receipt combined bytes overflow")?,
        native_records,
    ))
}

mod monitor_recovery_handoff;
#[cfg(test)]
pub(crate) use monitor_recovery_handoff::OriginalChildMonitorRecoveryDisposition;
pub(crate) use monitor_recovery_handoff::{
    observe_original_child_monitor_recovery_handoff_tx, OriginalChildMonitorRecoveryHandoff,
};
impl OriginalChildTaskTarget {
    pub(crate) fn recovery_delivery_id(&self) -> Option<&str> {
        self.delivery_id.as_deref()
    }
    pub(crate) fn recovery_turn_baseline(&self) -> u64 {
        self.turn_baseline
    }
}

mod spawn_no_start_handoff;
pub(crate) use spawn_no_start_handoff::{
    observe_original_child_spawn_no_start_handoff_tx, OriginalChildSpawnNoStartHandoff,
};
