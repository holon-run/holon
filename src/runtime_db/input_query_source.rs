//! Count the complete original birth interval; qualify bodies separately.
use super::*;
use crate::runtime::resident_input_evidence::{
    InputQueryCoverage, InputQueryError, InputQuerySelection,
};

pub(super) struct PreparedInputQuery {
    pub coverage: InputQueryCoverage,
    pub guards: Vec<Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>>,
}

pub(super) fn prepare_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    selection: &InputQuerySelection,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    limits: (usize, usize),
) -> Result<PreparedInputQuery> {
    ensure!(
        native_birth::installed(tx)?,
        InputQueryError::NeedsPreparation
    );
    let mut material = read_material::InputReadMaterial::new(scope, ledger, limits, None)?;
    // This creates only an empty metadata baseline, never historical membership.
    tx.execute(
        "INSERT INTO input_query_birth_cursors(agent_id,identity_incarnation,high_water) VALUES(?1,?2,0) ON CONFLICT(agent_id,identity_incarnation) DO NOTHING",
        params![scope.agent_id, scope.identity_incarnation],
    )?;
    let cursor: u64 = tx.query_row(
        "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation], |row| row.get(0),
    )?;
    let highest: u64 = tx.query_row(
        "SELECT COALESCE(MAX(birth_seq),0) FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id, scope.identity_incarnation], |row| row.get(0),
    )?;
    ensure!(highest == cursor, InputQueryError::Incomplete);
    let (rows, raw): (usize, usize) = if let Some(after) = selection.birth_after {
        ensure!(after <= cursor, InputQueryError::FrontierNotPrepared);
        let counts = tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(family AS BLOB))+length(CAST(record_id AS BLOB))),0) FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 AND birth_seq<=?4",
            params![scope.agent_id, scope.identity_incarnation, after, cursor],
            |row| Ok((row.get::<_, usize>(0)?, row.get::<_, usize>(1)?)),
        )?;
        ensure!(
            u64::try_from(counts.0)? == cursor - after,
            InputQueryError::Incomplete
        );
        counts
    } else {
        (0, 0)
    };
    let selected_bytes = crate::runtime::input_evidence_budget::serialized_bytes(selection)?;
    let selected_nodes = selection
        .required_keys
        .len()
        .checked_mul(3)
        .and_then(|n| n.checked_add(4))
        .context("Input query selection nodes overflow")?;
    let nodes = rows
        .checked_add(selected_nodes)
        .and_then(|n| n.checked_add(16))
        .and_then(|n| n.checked_mul(8))
        .context("Input query metadata nodes overflow")?;
    let bytes = usize::try_from(selected_bytes)?
        .checked_add(raw)
        .and_then(|n| n.checked_add(1024))
        .and_then(|n| n.checked_mul(8))
        .and_then(|n| n.checked_add(nodes.checked_mul(256)?))
        .context("Input query metadata bytes overflow")?;
    crate::diagnostics::resource_io::admit_owned_metadata(u64::try_from(bytes)?, nodes)?;
    material.observe_native_footprint(bytes, nodes)?;
    let mut born = BTreeMap::new();
    if let Some(after) = selection.birth_after {
        let mut statement = tx.prepare(
            "SELECT birth_seq,family,record_id FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 AND birth_seq<=?4 ORDER BY birth_seq",
        )?;
        let mut selected = statement.query(params![
            scope.agent_id,
            scope.identity_incarnation,
            after,
            cursor
        ])?;
        while let Some(row) = selected.next()? {
            ensure!(born.len() < rows, InputQueryError::Incomplete);
            let seq = row.get::<_, u64>(0)?;
            let family = row.get::<_, String>(1)?;
            let id = row.get::<_, String>(2)?;
            ensure!(
                born.insert(seq, Key::new(Family::from_str(&family)?, id))
                    .is_none(),
                InputQueryError::Incomplete
            );
        }
    }
    let coverage = InputQueryCoverage {
        required_keys: selection.required_keys.clone(),
        birth_after: selection.birth_after,
        birth_cursor: cursor,
        born,
    };
    coverage.validate_selection(selection)?;
    let (_, _, guards) = material.retain();
    Ok(PreparedInputQuery { coverage, guards })
}
