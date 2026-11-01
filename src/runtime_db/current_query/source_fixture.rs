//! Legitimate pre-wait revocation; post-preparation corruption stays raw.
use anyhow::{ensure, Context, Result};
use rusqlite::params;

use crate::{
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
    runtime_db::{
        current_read_writes::{CurrentFamily, OriginalCurrentWrites},
        input_evidence_snapshot::native_birth::{original_material_limits, NativeInputWriteProof},
        input_evidence_writes::{acknowledge_input_evidence_tx, InputEvidenceCommit},
        RuntimeDb,
    },
};

pub(crate) fn remove_qualified_terminal_queue_for_fixture(
    db: &RuntimeDb,
    scope: &InputEvidenceScope,
    ledger: &InputEvidenceBudgetLedger,
    message_id: &str,
) -> Result<InputEvidenceCommit<()>> {
    db.transaction(|tx| {
        let mut native = NativeInputWriteProof::prepare_tx(
            tx,
            scope,
            ledger,
            original_material_limits(ledger)?,
            None,
            |_| Ok(()),
        )?
        .context("Native terminal Queue source")?;
        let current = native
            .with_input_phase(tx, ledger, None, |material| {
                OriginalCurrentWrites::prepare(tx, scope, material, |visit| {
                    visit(CurrentFamily::Queue, message_id)
                })
            })?
            .context("Current terminal Queue source")?;
        ensure!(
            tx.execute(
                "DELETE FROM queue_entries WHERE agent_id=?1 AND message_id=?2
                 AND status IN ('processed','dropped','aborted','quarantined')",
                params![scope.agent_id, message_id],
            )? == 1,
            "fixture removal requires one owned terminal Queue"
        );
        let current = native.with_input_phase(tx, ledger, None, |material| {
            current.finish(material, |visit| visit(CurrentFamily::Queue, message_id))
        })?;
        let mut commit = acknowledge_input_evidence_tx(tx, scope, (), &[])?;
        native.finish_delta_tx(tx, &mut commit.input_delta, ledger, None)?;
        commit.current_read_writes = Some(current);
        Ok(commit)
    })
}
