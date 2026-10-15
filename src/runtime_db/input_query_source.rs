//! Complete original native birth metadata, followed by the original body qualifier.
use super::*;
use crate::runtime::{
    input_evidence_budget::borrowed_json_material,
    input_queries::{InputQueryCoverage, InputQueryError, InputQuerySnapshot},
};

pub(super) struct InputSourceRead<'a> {
    pub(super) roles: Option<&'a InputQuerySnapshot>,
    pub(super) owner_epoch: u64,
}
#[derive(Default)]
pub(super) struct PreparedInputSource {
    pub(super) roots: BTreeSet<Key>,
    pub(super) coverage: InputQueryCoverage,
}
pub(super) fn prepare_tx(
    tx: &Transaction<'_>, scope: &InputEvidenceScope, source: InputSourceRead<'_>,
    material: &mut read_material::InputReadMaterial<'_>,
) -> Result<PreparedInputSource> {
    material.validate_binding(scope)?;
    if let Some(roles) = source.roles {
        material.observe_query_prior(roles, source.owner_epoch)?;
    }
    ensure!(native_birth::installed(tx)?, InputQueryError::Incomplete);
    // Only the named actor reaches this after its actual current identity/State
    // check. A zero baseline does not adopt any canonical body or raw address.
    native_birth::initialize(tx, scope)?;
    let cursor: u64 = tx.query_row(
        "SELECT high_water FROM input_query_birth_cursors WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation], |row|row.get(0),
    )?;
    let highest: u64 = tx.query_row(
        "SELECT COALESCE(MAX(birth_seq),0) FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2",
        params![scope.agent_id,scope.identity_incarnation], |row|row.get(0),
    )?;
    ensure!(highest == cursor, InputQueryError::Incomplete);
    let selection = source.roles.map(|roles| &roles.selection);
    let after = selection.and_then(|selection| selection.birth_after);
    ensure!(after.is_none_or(|after| after <= cursor), InputQueryError::FrontierNotPrepared);
    let (rows, raw): (usize, usize) = if let Some(after) = after {
        tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(CAST(family AS BLOB))+length(CAST(record_id AS BLOB))+length(CAST(birth_seq AS BLOB))),0) FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 AND birth_seq<=?4",
            params![scope.agent_id,scope.identity_incarnation,after,cursor], |row|Ok((row.get(0)?,row.get(1)?)),
        )?
    } else { (0, 0) };
    ensure!(after.is_none_or(|after| u64::try_from(rows).ok() == cursor.checked_sub(after)),
        InputQueryError::Incomplete);
    // All rows are counted; there is no history LIMIT and no rowid-derived
    // authority. Admit Strings, both maps/sets, Arc slots and native containers.
    let (selected_bytes, selected_nodes) = borrowed_json_material(&(scope, selection))?;
    let nodes = rows.checked_mul(6).and_then(|n|n.checked_add(selected_nodes))
        .and_then(|n|n.checked_add(16)).and_then(|n|n.checked_mul(8))
        .context(InputQueryError::ResourceExhausted("birth metadata"))?;
    let bytes = u64::try_from(raw)?.checked_add(selected_bytes)
        .and_then(|n|n.checked_add(1024)).and_then(|n|n.checked_mul(8))
        .and_then(|n|n.checked_add(u64::try_from(nodes).ok()?.checked_mul(256)?))
        .context(InputQueryError::ResourceExhausted("birth metadata"))?;
    material.observe_projected(bytes, nodes)?;
    let mut roots = selection.map(|selection| selection.required_keys.clone()).unwrap_or_default();
    let mut born = BTreeMap::new();
    if let Some(after) = after {
        let mut statement = tx.prepare(
            "SELECT family,record_id,birth_seq FROM input_query_birth_members WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 AND birth_seq<=?4 ORDER BY birth_seq",
        )?;
        let mut selected = statement.query(params![scope.agent_id,scope.identity_incarnation,after,cursor])?;
        let mut copied = 0usize;
        while let Some(row) = selected.next()? {
            copied = copied.checked_add(1).context(InputQueryError::ResourceExhausted("birth count"))?;
            ensure!(copied <= rows, "Input birth metadata changed within original transaction");
            let family: String = row.get(0)?;
            let key = Key::new(Family::from_str(&family)?, row.get::<_,String>(1)?);
            let seq: u64 = row.get(2)?;
            ensure!(seq > after && seq <= cursor && born.insert(key.clone(), seq).is_none(),
                "Input birth metadata duplicated or crossed its accepted frontier");
            roots.insert(key);
        }
        ensure!(copied == rows, "Input birth metadata changed within original transaction");
    }
    Ok(PreparedInputSource {
        roots,
        coverage: InputQueryCoverage {
            required_keys: selection.map(|selection| selection.required_keys.clone()).unwrap_or_default(),
            birth_cursor: Some(cursor), birth_after: after, born_key_seqs: born,
        },
    })
}

#[cfg(test)]
#[path = "input_query_source_tests.rs"]
mod tests;
