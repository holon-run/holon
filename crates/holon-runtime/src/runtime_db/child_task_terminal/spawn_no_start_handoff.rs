//! No new journal or Ready: the complete original Task retains an unstarted command.
use super::OriginalChildTaskAddress;
use crate::runtime::{
    input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
    OriginalChildSpawnNoStart, OriginalChildTaskProducer,
};
use crate::runtime_db::{command_task_terminal as owner, task_snapshot as tasks};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Transaction};
use std::sync::Arc;
pub(crate) struct OriginalChildSpawnNoStartHandoff {
    original: OriginalChildTaskAddress,
    _guards: Vec<Arc<BudgetViewGuard>>,
}
impl OriginalChildSpawnNoStartHandoff {
    pub(crate) fn matches_original(&self, source: &OriginalChildTaskProducer) -> bool {
        &self.original == source.address()
    }
}
pub(crate) fn observe_original_child_spawn_no_start_handoff_tx(
    tx: &Transaction<'_>,
    source: &OriginalChildTaskProducer,
    token: &OriginalChildSpawnNoStart,
    request: &tasks::TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<Option<OriginalChildSpawnNoStartHandoff>> {
    source.validate_material_ledger(ledger)?;
    ensure!(
        token.matches_original(source),
        "unstarted recovery token differs"
    );
    let scope = &source.address().scope;
    let mut phase = crate::runtime_db::task_status_metadata::PhaseMaterial::new(request);
    let observed = phase.owner_tx(tx, scope, ledger)?;
    if observed.mode != owner::CommandTaskTerminalInstallMode::Current {
        return Ok(None);
    };
    let mut guards = observed.into_material();
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
    let bytes = usize::try_from(crate::runtime::input_evidence_budget::serialized_bytes(
        source.address(),
    )?)?
    .checked_add(512)
    .and_then(|n| n.checked_mul(8))
    .context("unstarted recovery selector overflow")?;
    charge(&mut phase, &mut guards, scope, ledger, bytes, 16)?;
    let keys = std::collections::BTreeSet::from([source.address().admitted.key.clone()]);
    let raw = tasks::task_maintenance_material_tx(tx, scope, &keys)?;
    let coverage:usize=tx.query_row("SELECT COALESCE((SELECT length(CAST(coverage_json AS BLOB)) FROM task_domain_current WHERE agent_id=?1 AND identity_incarnation=?2),0)",params![scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        raw.bytes
            .checked_add(
                coverage
                    .checked_mul(8)
                    .context("unstarted coverage copies overflow")?,
            )
            .context("unstarted raw bytes overflow")?,
        raw.records,
    )?;
    let nodes:usize=tx.query_row("SELECT (SELECT COUNT(*) FROM tasks t,json_tree(t.payload_json) WHERE t.task_id=?1 AND t.owner_agent_id=?2)+(SELECT COUNT(*) FROM task_domain_current d,json_tree(d.coverage_json) WHERE d.agent_id=?2 AND d.identity_incarnation=?3)",params![source.address().admitted.key.id,scope.agent_id,scope.identity_incarnation],|row|row.get(0))?;
    let records = nodes
        .checked_add(128)
        .and_then(|n| n.checked_mul(8))
        .context("unstarted native records overflow")?;
    charge(
        &mut phase,
        &mut guards,
        scope,
        ledger,
        records
            .checked_mul(256)
            .context("unstarted native bytes overflow")?,
        records,
    )?;
    let qualified = tasks::prepare_existing_task_maintenance_tx(tx, scope, &keys)?;
    let task = qualified.qualified_task_before(&source.address().admitted.key.id)?;
    token.validate_durable_task(source, task)?;
    Ok(Some(OriginalChildSpawnNoStartHandoff {
        original: source.address().clone(),
        _guards: guards,
    }))
}
