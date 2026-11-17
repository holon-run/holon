//! Explicit Task-ready opt-in for tests; State-only fixtures stay State-only.

use super::task_snapshot::{
    prepare_task_domain_snapshot_with_budget_tx, TaskCurrentSnapshot, TaskDomainLoadRequest,
};
use crate::runtime::{
    input_evidence_budget::InputEvidenceBudgetLedger, resident_input_evidence::InputEvidenceScope,
};
use anyhow::{Context, Result};
use rusqlite::Transaction;

/// Prepare in the caller's transaction with its exact capture/recovery selection
/// and budget. This does not seed Tasks, qualify writes, or retry failures.
pub(crate) fn prepare_current_task_fixture_tx(
    tx: &Transaction<'_>,
    scenario: &str,
    scope: &InputEvidenceScope,
    request: &TaskDomainLoadRequest,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<TaskCurrentSnapshot> {
    prepare_task_domain_snapshot_with_budget_tx(tx, scope, request, ledger)
        .and_then(|snapshot| snapshot.require_current())
        .with_context(|| {
            format!(
                "Task fixture {scenario}: scope {} incarnation {}",
                scope.agent_id, scope.identity_incarnation
            )
        })
}

mod task_fixture_tests;
