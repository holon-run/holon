//! Only a synchronous, discarded original preflight can release its own scratch.
use super::*;

pub(in crate::runtime_db::task_snapshot) struct TaskPreflightMark {
    bytes: usize,
    records: usize,
    guards: usize,
    original: std::sync::Weak<BudgetViewGuard>,
}
pub(in crate::runtime_db) struct ReleasedTaskPreflightMaterial<'a> {
    scope: &'a InputEvidenceScope,
    ledger: &'a InputEvidenceBudgetLedger,
    bytes: usize,
    records: usize,
}
impl ReleasedTaskPreflightMaterial<'_> {
    pub(in crate::runtime_db) fn validate_accounting(
        &self,
        scope: &InputEvidenceScope,
        ledger: &InputEvidenceBudgetLedger,
    ) -> Result<(usize, usize)> {
        ensure!(
            self.scope == scope && self.ledger.same_ledger(ledger),
            "Task preflight release differs from original Memory scope/ledger"
        );
        Ok((self.bytes, self.records))
    }
}
impl TaskReadMaterial<'_> {
    pub(in crate::runtime_db::task_snapshot) fn mark_preflight(&self) -> Result<TaskPreflightMark> {
        Ok(TaskPreflightMark {
            bytes: self.bytes,
            records: self.records,
            guards: self.guards.len(),
            original: Arc::downgrade(
                self.guards
                    .first()
                    .context("Task preflight original guard missing")?,
            ),
        })
    }
    pub(in crate::runtime_db::task_snapshot) fn retire_preflight(
        &mut self,
        mark: TaskPreflightMark,
    ) -> Result<()> {
        let original = Arc::downgrade(
            self.guards
                .first()
                .context("Task preflight original guard missing")?,
        );
        ensure!(
            std::sync::Weak::ptr_eq(&mark.original, &original) && mark.guards <= self.guards.len(),
            "Task preflight mark belongs to another phase"
        );
        ensure!(
            self.guards[mark.guards..]
                .iter()
                .all(|guard| Arc::strong_count(guard) == 1),
            "Task preflight guard has another live holder"
        );
        let ledger = self
            .budget
            .context("Task preflight requires actual ledger")?;
        let released = ReleasedTaskPreflightMaterial {
            scope: self.scope,
            ledger,
            bytes: self
                .bytes
                .checked_sub(mark.bytes)
                .context("Task preflight byte underflow")?,
            records: self
                .records
                .checked_sub(mark.records)
                .context("Task preflight node underflow")?,
        };
        for guard in &self.guards[mark.guards..] {
            guard.validate_owned_material_binding(self.scope, ledger, 0)?;
        }
        if let Some(memory) = self.memory_budget.as_deref_mut() {
            memory.release_task_preflight_material(released)?;
        }
        self.guards.truncate(mark.guards);
        self.bytes = mark.bytes;
        self.records = mark.records;
        Ok(())
    }
}

#[cfg(test)]
#[path = "preflight_material_tests.rs"]
mod tests;
