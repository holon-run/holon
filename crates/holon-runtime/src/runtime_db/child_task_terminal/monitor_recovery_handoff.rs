//! Relinquish only a finished local wait whose complete original recovery is durable.
use super::{OriginalChildTaskAddress, OriginalChildTaskTarget};
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    OriginalChildTaskProducer,
};
use crate::runtime_db::{command_task_terminal as owner, task_snapshot as tasks};
use anyhow::{Context, Result};
use rusqlite::{params, Transaction};
use std::sync::Arc;
/// This records the bootstrap responsibility, not a resumed monitor or Ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OriginalChildMonitorRecoveryDisposition {
    RecheckExistingTarget,
    InterruptedUnavailableTarget,
}
pub(crate) struct OriginalChildMonitorRecoveryHandoff {
    original: OriginalChildTaskAddress,
    _disposition: OriginalChildMonitorRecoveryDisposition,
    _guards: Vec<Arc<BudgetViewGuard>>,
    _target: Arc<OriginalChildTaskTarget>,
}
impl OriginalChildMonitorRecoveryHandoff {
    pub(crate) fn matches_original(&self, source: &OriginalChildTaskProducer) -> bool {
        &self.original == source.address()
    }
    #[cfg(test)]
    pub(crate) fn disposition(&self) -> OriginalChildMonitorRecoveryDisposition {
        self._disposition
    }
}
pub(crate) fn observe_original_child_monitor_recovery_handoff_tx(
    tx: &Transaction<'_>,
    source: &OriginalChildTaskProducer,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<OriginalChildMonitorRecoveryHandoff>> {
    source.validate_material_ledger(ledger)?;
    let Some(target) = source.monitor_recovery_target()? else {
        return Ok(None);
    };
    let scope = &source.address().scope;
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    let mut guards = Vec::new();
    fn charge(
        phase: &mut crate::runtime_db::task_status_metadata::PhaseMaterial<'_>,
        guards: &mut Vec<Arc<BudgetViewGuard>>,
        scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
        bytes: usize,
        records: usize,
    ) -> Result<()> {
        guards.push(owner::charge_material(
            scope,
            bytes,
            records,
            &phase.remaining()?,
            ledger,
        )?);
        phase.include(bytes, records)
    }
    // This common observer validates the original incarnation before State presence;
    // no State body, successor State or Runtime construction is used here.
    let owner_bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(payload_json AS BLOB))+length(CAST(agent_id AS BLOB))+length(CAST(status AS BLOB)) FROM agent_identities WHERE agent_id=?1),0)",[&scope.agent_id],|r|r.get(0))?;
    let owner_bytes = owner_bytes
        .checked_add(scope.agent_id.len() + 256)
        .and_then(|n| n.checked_mul(8))
        .context("monitor recovery owner bytes overflow")?;
    let observed = owner::observe_original_owner_tx(tx, scope, &phase.remaining()?, ledger)?;
    phase.include(owner_bytes, 8)?;
    if observed.mode != owner::CommandTaskTerminalInstallMode::Current {
        return Ok(None);
    };
    guards.extend(observed.into_material());
    let selector_bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
        source.address(),
    )?)?
    .checked_add(512)
    .and_then(|n| n.checked_mul(8))
    .context("monitor recovery selector overflow")?;
    charge(&mut phase, &mut guards, scope, ledger, selector_bytes, 16)?;
    let keys = std::collections::BTreeSet::from([source.address().admitted.key.clone()]);
    let material = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
    let coverage_bytes:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2),0)",params![scope.agent_id,scope.identity_incarnation],|r|r.get(0))?;
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        material
            .bytes
            .checked_add(
                coverage_bytes
                    .checked_mul(8)
                    .context("monitor coverage bytes overflow")?,
            )
            .context("monitor Task bytes overflow")?,
        material.records,
    )?;
    let nodes:usize=tx.query_row("SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1 AND t.owner_agent_id=?2",params![source.address().admitted.key.id,scope.agent_id],|r|r.get(0))?;
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(8 * 256))
            .context("monitor Task node bytes overflow")?,
        nodes
            .checked_add(128)
            .and_then(|n| n.checked_mul(8))
            .context("monitor Task nodes overflow")?,
    )?;
    let before = tasks::prepare_existing_task_maintenance_tx(tx, scope, &keys)?;
    let task = before.qualified_task_before(&source.address().admitted.key.id)?;
    // A full current canonical before is required; raw tail Task or a selector
    // alone can never discharge the original monitor responsibility.
    if !source.validate_durable_monitor_recovery(task, &target)? {
        return Ok(None);
    };
    let target_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1)",
        [target.target_id()],
        |row| row.get(0),
    )?;
    if !target_exists {
        // The complete exact Task still owns target/baseline/delivery/artifact.
        // Bootstrap's existing !reusable branch retains it as Interrupted;
        // absence is not proof of a child result or filesystem cleanup.
        return Ok(Some(OriginalChildMonitorRecoveryHandoff {
            original: source.address().clone(),
            _disposition: OriginalChildMonitorRecoveryDisposition::InterruptedUnavailableTarget,
            _guards: guards,
            _target: target,
        }));
    }
    let fresh = super::bind_original_child_target_tx(
        tx,
        source,
        target.target_id(),
        target.recovery_delivery_id(),
        target.recovery_turn_baseline(),
        &phase.remaining()?,
        ledger,
    )?;
    if !target.same_target(&fresh) {
        anyhow::bail!("monitor recovery target changed its original incarnation/association")
    };
    if !source.validate_durable_monitor_recovery(task, &fresh)? {
        return Ok(None);
    };
    let active: bool = tx.query_row(
        "SELECT status='active' FROM agent_identities WHERE agent_id=?1",
        [target.target_id()],
        |row| row.get(0),
    )?;
    let disposition = if active {
        OriginalChildMonitorRecoveryDisposition::RecheckExistingTarget
    } else {
        OriginalChildMonitorRecoveryDisposition::InterruptedUnavailableTarget
    };
    Ok(Some(OriginalChildMonitorRecoveryHandoff {
        original: source.address().clone(),
        _disposition: disposition,
        _guards: guards,
        _target: fresh,
    }))
}
