//! Native birth metadata is independent of finite ordinary Event body windows.
use super::*;

pub(super) fn load_tx(
    tx: &Transaction<'_>,
    scope: &InputEvidenceScope,
    after: Option<u64>,
    material: &mut Material<'_>,
) -> Result<CurrentEventQueryCoverage> {
    let cursor = writes::original_native_event_cursor_tx(tx, scope)?;
    if let Some(cursor) = cursor {
        let valid:bool=tx.query_row("SELECT COALESCE(MAX(birth_seq),0)<=?3 FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2",params![scope.agent_id,scope.identity_incarnation,cursor],|r|r.get(0))?;
        ensure!(valid, CurrentReadError::CanonicalChanged);
    }
    let mut query = CurrentEventQueryCoverage {
        birth_cursor: cursor,
        birth_after: after,
        born_event_seqs: BTreeMap::new(),
    };
    let Some(after) = after else { return Ok(query) };
    let cursor = cursor.ok_or(CurrentReadError::NeedsPreparation)?;
    ensure!(after <= cursor, CurrentReadError::RevisionMismatch);
    // Check both candidate ranges: a corrupted birth may move above the cut,
    // or below it while the actual qualified member still belongs after it.
    let drift:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM current_event_native_births b LEFT JOIN current_read_members m ON m.agent_id=b.agent_id AND m.identity_incarnation=b.identity_incarnation AND m.family='event' AND m.record_id=b.event_id WHERE b.agent_id=?1 AND b.identity_incarnation=?2 AND b.birth_seq>?3 AND (m.record_id IS NULL OR m.sort_seq!=b.birth_seq)) OR EXISTS(SELECT 1 FROM current_read_members m JOIN current_event_native_births b ON b.agent_id=m.agent_id AND b.identity_incarnation=m.identity_incarnation AND b.event_id=m.record_id WHERE m.agent_id=?1 AND m.identity_incarnation=?2 AND m.family='event' AND m.sort_seq>?3 AND b.birth_seq<=?3)",params![scope.agent_id,scope.identity_incarnation,after],|r|r.get(0))?;
    ensure!(!drift, CurrentReadError::CanonicalChanged);
    // Only the original actual ACK source chooses addresses. The caller still
    // validates every exact member and body through the original closed loader.
    let (count,bytes):(usize,usize)=tx.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(event_id AS BLOB))+length(CAST(birth_seq AS BLOB))+256),0) FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3",
        params![scope.agent_id,scope.identity_incarnation,after],|r|Ok((r.get(0)?,r.get(1)?)))?;
    material.charge(
        bytes,
        count
            .checked_mul(4)
            .and_then(|n| n.checked_add(8))
            .context("Event query source count overflow")?,
    )?;
    let mut statement=tx.prepare("SELECT event_id,birth_seq FROM current_event_native_births WHERE agent_id=?1 AND identity_incarnation=?2 AND birth_seq>?3 ORDER BY birth_seq")?;
    let mut rows = statement.query(params![scope.agent_id, scope.identity_incarnation, after])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let seq: u64 = row.get(1)?;
        ensure!(
            seq > after && seq <= cursor,
            CurrentReadError::CanonicalChanged
        );
        ensure!(
            query.born_event_seqs.insert(id, seq).is_none(),
            CurrentReadError::CanonicalChanged
        );
    }
    ensure!(
        query.born_event_seqs.len() == count,
        CurrentReadError::CanonicalChanged
    );
    query.validate()?;
    Ok(query)
}
