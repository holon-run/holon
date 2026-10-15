//! The original child follow-up borrows one immutable parent request allocation.
use super::{
    child_task_terminal::OriginalChildTaskSourceView,
    input_evidence_budget::{borrowed_json_material, BudgetViewGuard, InputEvidenceBudgetLedger},
    resident_input_evidence::InputEvidenceScope,
    RuntimeHandle,
};
use crate::types::{
    AdmissionContext, AgentIdentityRecord, AgentKind, AgentOwnership, AgentRegistryStatus,
    AgentVisibility, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageKind,
    MessageOrigin, TaskRecord,
};
use anyhow::{ensure, Context, Result};
use std::sync::Arc;

/// Finite identity address only. The accepted target/source, Root and old child
/// Runtime are not retained, so idle replacement of the same identity is lawful.
pub(crate) struct OriginalTaskInputChildBinding {
    parent: InputEvidenceScope,
    task_id: String,
    child: InputEvidenceScope,
    ledger: InputEvidenceBudgetLedger,
    usage: (u64, usize),
    _request: Arc<BudgetViewGuard>,
}
impl std::fmt::Debug for OriginalTaskInputChildBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalTaskInputChildBinding")
            .field("parent", &self.parent)
            .field("task_id", &self.task_id)
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}
impl OriginalTaskInputChildBinding {
    /// Caller admits these finite address copies into its existing request
    /// guard before this constructor. A task ID hint never mints this binding.
    pub(in crate::runtime) fn from_original_target(
        parent: &RuntimeHandle,
        task: &TaskRecord,
        source: &OriginalChildTaskSourceView<'_>,
        guard: Arc<BudgetViewGuard>,
        usage: (u64, usize),
    ) -> Result<Option<Arc<Self>>> {
        source.validate_generation(task)?;
        let original = source.address();
        ensure!(
            original.scope.agent_id == parent.instance_agent_id()
                && original.scope.identity_incarnation == parent.instance_incarnation()
                && original.owner_epoch == parent.instance_owner_epoch(),
            "TaskInput child source belongs to another parent owner"
        );
        let Some(target) = source
            .target()
            .filter(|target| target.has_created_child_duty())
        else {
            return Ok(None);
        };
        let Some(child) = target.target_scope() else {
            return Ok(None);
        };
        ensure!(
            target.original() == original
                && target.target_id() == child.agent_id
                && task.is_child_agent_task()
                && task
                    .detail
                    .as_ref()
                    .and_then(|d| d.get("child_agent_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(child.agent_id.as_str()),
            "TaskInput child differs from its original accepted target"
        );
        let ledger = &parent.inner.input_evidence_budget;
        guard.validate_host_owned_material_binding(ledger, usage.0)?;
        ensure!(
            guard.owned_material_bytes()? == usage.0,
            "TaskInput original request allocation changed before sealing"
        );
        let (bytes, nodes) = borrowed_json_material(&(&original.scope, &task.id, child))?;
        let floor = bytes
            .checked_add(4096)
            .and_then(|n| n.checked_add((nodes as u64).checked_mul(256)?))
            .context("TaskInput child address material overflow")?;
        ensure!(
            usage.0 >= floor && usage.1 >= nodes.saturating_add(32),
            "TaskInput child address was not admitted before copying"
        );
        Ok(Some(Arc::new(Self {
            parent: original.scope.clone(),
            task_id: task.id.clone(),
            child: child.clone(),
            ledger: ledger.clone(),
            usage,
            _request: guard,
        })))
    }
    pub(crate) fn child_scope(&self) -> &InputEvidenceScope {
        &self.child
    }
    pub(crate) fn parent_id(&self) -> &str {
        &self.parent.agent_id
    }
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }
    pub(crate) fn validate_identity(&self, identity: &AgentIdentityRecord) -> Result<()> {
        ensure!(
            identity.agent_id == self.child.agent_id
                && identity.incarnation == self.child.identity_incarnation
                && identity.status == AgentRegistryStatus::Active
                && identity.kind == AgentKind::Child
                && identity.visibility == AgentVisibility::Private
                && identity.ownership() == AgentOwnership::ParentSupervised
                && identity.parent_agent_id.as_deref() == Some(self.parent.agent_id.as_str())
                && identity.delegated_from_task_id.as_deref() == Some(self.task_id.as_str()),
            "TaskInput child identity differs from its original parent/task incarnation"
        );
        Ok(())
    }
    /// Every retry binds only the replacement Runtime's actual epoch. The
    /// durable original child incarnation and parent/task association stay fixed.
    pub(crate) fn for_attempt(
        self: &Arc<Self>,
        child: &RuntimeHandle,
        authority: AuthorityClass,
    ) -> Result<Arc<OriginalTaskInputChildMaterial>> {
        ensure!(
            child.instance_agent_id() == self.child.agent_id
                && child.instance_incarnation() == self.child.identity_incarnation
                && self.ledger.same_ledger(&child.inner.input_evidence_budget),
            "TaskInput retry crossed the original child incarnation/ledger"
        );
        self.validate_request_guard()?;
        Ok(Arc::new(OriginalTaskInputChildMaterial {
            binding: self.clone(),
            owner_epoch: child.instance_owner_epoch(),
            authority,
        }))
    }
    fn validate_request_guard(&self) -> Result<()> {
        self._request
            .validate_host_owned_material_binding(&self.ledger, self.usage.0)?;
        ensure!(
            self._request.owned_material_bytes()? == self.usage.0,
            "TaskInput sealed original request guard changed"
        );
        Ok(())
    }
}

/// Accounting only, not an Input/Task/Stop/physical capability. Host allocations
/// stay Host allocations and are never relabeled as child agent guard entries.
pub(crate) struct OriginalTaskInputChildMaterial {
    binding: Arc<OriginalTaskInputChildBinding>,
    owner_epoch: u64,
    authority: AuthorityClass,
}
impl std::fmt::Debug for OriginalTaskInputChildMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OriginalTaskInputChildMaterial")
            .field("binding", &self.binding)
            .field("owner_epoch", &self.owner_epoch)
            .finish_non_exhaustive()
    }
}
impl OriginalTaskInputChildMaterial {
    pub(crate) fn child_scope(&self) -> &InputEvidenceScope {
        &self.binding.child
    }
    pub(crate) fn validate_binding(
        &self,
        scope: &InputEvidenceScope,
        epoch: u64,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<()> {
        ensure!(
            scope == &self.binding.child
                && epoch == self.owner_epoch
                && ledger.same_ledger(&self.binding.ledger),
            "TaskInput child material crossed its original attempt scope/epoch/ledger"
        );
        self.binding.validate_request_guard()
    }
    pub(crate) fn material_estimate(&self) -> Result<(usize, usize)> {
        self.binding.validate_request_guard()?;
        Ok((usize::try_from(self.binding.usage.0)?, self.binding.usage.1))
    }
    pub(crate) fn validate_identity(&self, identity: &AgentIdentityRecord) -> Result<()> {
        self.binding.validate_identity(identity)
    }
    pub(crate) fn validate_message(&self, message: &MessageEnvelope) -> Result<()> {
        let text = |key| {
            message
                .metadata
                .as_ref()
                .and_then(|m| m.get(key))
                .and_then(serde_json::Value::as_str)
        };
        ensure!(
            message.agent_id == self.binding.child.agent_id
                && message.kind == MessageKind::InternalFollowup
                && message.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
                && message.admission_context == Some(AdmissionContext::RuntimeOwned)
                && message.authority_class == self.authority
                && matches!(&message.origin, MessageOrigin::Task{task_id} if task_id == &self.binding.task_id)
                && text("delegated_task_id") == Some(self.binding.task_id.as_str())
                && text("parent_agent_id") == Some(self.binding.parent.agent_id.as_str())
                && text("child_agent_id") == Some(self.binding.child.agent_id.as_str())
                && text("followup_via") == Some("task_input"),
            "TaskInput child material has another original follow-up"
        );
        Ok(())
    }
    pub(crate) fn visit_material(
        &self,
        mut visit: impl FnMut(&Arc<BudgetViewGuard>),
    ) -> Result<()> {
        self.binding.validate_request_guard()?;
        visit(&self.binding._request);
        Ok(())
    }
}
