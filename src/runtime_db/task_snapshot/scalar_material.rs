//! Only the original scalar consumers may discard their complete decoded read.
use super::*;
impl TaskReadMaterial<'_> {
    fn scalar_mark(&mut self) -> Result<preflight_material::TaskPreflightMark> {
        if self.guards.is_empty() {
            // Material::with_task_material can lend an earlier phase's counters
            // while that caller holds its guards. This real local anchor owns
            // only its guard/scope/bookkeeping and never grants a body.
            let bytes = self
                .scope
                .agent_id
                .len()
                .checked_add(512)
                .context("Task scalar phase anchor overflow")?;
            self.charge_writer_material(bytes, 4, 1)?;
        }
        self.mark_preflight()
    }
    pub(in crate::runtime_db::task_snapshot) fn original_revision_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
    ) -> Result<u64> {
        if self.budget.is_none() {
            return Ok(self
                .read_current_metadata_for_scope(tx, scope)?
                .map_or(1, |coverage| coverage.revision));
        }
        let mark = self.scalar_mark()?;
        let coverage = self.read_current_metadata_for_scope(tx, scope)?;
        let revision = coverage.as_ref().map_or(1, |coverage| coverage.revision);
        drop(coverage);
        self.retire_preflight(mark)?;
        Ok(revision)
    }
    pub(in crate::runtime_db) fn original_maintenance_size_tx(
        &mut self,
        tx: &Transaction<'_>,
        scope: &InputEvidenceScope,
        keys: &BTreeSet<TaskKey>,
    ) -> Result<TaskMaintenanceMaterial> {
        if self.budget.is_none() {
            return task_maintenance_material_tx(tx, scope, keys);
        }
        let mark = self.scalar_mark()?;
        for key in keys {
            self.admit_projection_size_tx(tx, key)?;
        }
        let actual = task_maintenance_material_tx(tx, scope, keys)?;
        // All projection statements returned integer sizes; no body escapes.
        self.retire_preflight(mark)?;
        Ok(actual)
    }
}
#[cfg(test)]
#[path = "scalar_material_tests.rs"]
mod tests;
